// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The OS boundary.
//!
//! Every `#[cfg]` and every `unsafe` block in this tree lives here, so a port is
//! a matter of implementing this file rather than auditing the whole crate.
//!
//! Linux is the verified target. macOS compiles and its branches are written,
//! but nothing claims it works until someone runs the suite on a Mac. What the
//! two share — `exec`, `flock`, `isatty`, the signal-to-exit-code mapping — is
//! identical, so the only real divergence is where user directories live.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use crate::static_assert;

/// Exit code a shell reports for a child killed by `signum`.
///
/// The child's fate is reported unchanged, and a signal death is reported the
/// way every shell reports it.
pub fn exit_code_for_signal(signum: i32) -> i32 {
    128 + signum
}

/// The exit code Lens should exit with, given a child's status.
///
/// Normal exit propagates the code. Signal death becomes `128 + signum`. A
/// status carrying neither (which the platform should not produce) becomes 1,
/// because reporting success for a child whose fate is unknown is the one
/// answer that is certainly wrong.
pub fn exit_code_for_status(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signum) = status.signal() {
            return exit_code_for_signal(signum);
        }
    }
    1
}

/// Replace this process with `cmd`.
///
/// This is how passthrough stays honest. Spawning and copying
/// streams would be an imitation of the command; replacing the process image
/// *is* the command — same stdio, same terminal control, same exit code, same
/// signal disposition, with Lens no longer in the picture at all.
///
/// # Errors
///
/// Returns the `io::Error` from `execvp` if the image could not be replaced. On
/// success it does not return.
#[cfg(unix)]
pub fn exec(cmd: &mut Command) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    // `exec` consumes the process on success, so the returned error is the only
    // outcome the caller can observe.
    cmd.exec()
}

/// The outcome of [`passthrough`].
pub enum Passthrough {
    /// The child could not be started at all — `cmd.exec()` failed on Unix, or
    /// `cmd.spawn()` failed on the imitation fallback below.
    FailedToStart(std::io::Error),
    /// Non-Unix only: there is no process-image replacement to fall back to,
    /// so the child was spawned and waited on instead, and this is how it
    /// exited.
    Completed(std::process::ExitStatus),
}

/// Run `cmd` in place of this process, on whatever platform allows that.
///
/// On Unix this really does replace the process image via [`exec`]; a caller
/// distinguishing "started fine" from "failed to start" needs nothing beyond
/// the `Err` case, because success never returns. No target this tree
/// verifies lacks `exec`, so the non-Unix branch is unverified by the same
/// decision that leaves the rest of that platform unverified — see the module
/// docs. It spawns and waits, which is an imitation of passthrough rather than
/// passthrough itself, but it is the only thing available without an `exec`.
#[cfg(unix)]
pub fn passthrough(cmd: &mut Command) -> Passthrough {
    Passthrough::FailedToStart(exec(cmd))
}

/// See the Unix version of [`passthrough`] for the contract.
#[cfg(not(unix))]
pub fn passthrough(cmd: &mut Command) -> Passthrough {
    match cmd.status() {
        Ok(status) => Passthrough::Completed(status),
        Err(err) => Passthrough::FailedToStart(err),
    }
}

/// Set the name the child sees as its own `argv[0]`.
///
/// Left as the user typed it rather than as Lens resolved it, so a command
/// that inspects its own name behaves the way it would outside Lens.
#[cfg(unix)]
pub fn set_arg0(cmd: &mut Command, arg0: &str) {
    use std::os::unix::process::CommandExt;
    cmd.arg0(arg0);
}

/// See the Unix version of [`set_arg0`] for the contract.
///
/// The standard library has no portable way to set `argv[0]` apart from the
/// program path, so on a platform without the Unix extension the child sees
/// the resolved program name instead. This only matters to a command that
/// inspects its own `argv[0]`, and no verified target reaches this branch.
#[cfg(not(unix))]
pub fn set_arg0(_cmd: &mut Command, _arg0: &str) {}

/// Is `fd` a terminal?
///
/// Used to decide whether a child may take over the terminal. A wrong answer
/// here is expensive in one direction only: capturing an
/// interactive command hangs the user, while passing an inert one through just
/// forgoes filtering. Callers resolve doubt toward passthrough.
#[cfg(unix)]
pub fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty(3) reads only the descriptor table entry for `fd`. It
    // dereferences no caller memory, accepts any int (returning 0 with EBADF
    // for a closed or invalid descriptor), and has no side effects.
    unsafe { isatty(fd) == 1 }
}

/// Standard file descriptor numbers, as passed to [`is_tty`].
pub const STDIN_FD: i32 = 0;

// The one language boundary in this crate. Two libc calls, declared in-tree
// rather than pulling in a crate for them: `isatty` for interactive detection
// and `flock` for log rotation.
//
// `c_int` is the ABI contract for both. Asserting its width at compile time is
// cheap and catches a target where the assumption silently stops holding.
static_assert!(core::mem::size_of::<std::os::raw::c_int>() == 4);
static_assert!(core::mem::align_of::<std::os::raw::c_int>() == 4);

#[cfg(unix)]
unsafe extern "C" {
    fn isatty(fd: std::os::raw::c_int) -> std::os::raw::c_int;
    fn flock(fd: std::os::raw::c_int, operation: std::os::raw::c_int) -> std::os::raw::c_int;
}

/// `flock(2)` operations. Values are fixed by the ABI, not chosen by us, and a
/// typo in one turns "take the lock" into "release it". Linux
/// (asm-generic/fcntl.h) and macOS (sys/file.h) agree on all three.
#[cfg(unix)]
pub mod lock_op {
    /// Exclusive lock.
    pub const EX: i32 = 2;
    /// Unlock.
    pub const UN: i32 = 8;
    /// Fail rather than block.
    pub const NB: i32 = 4;
}

#[cfg(unix)]
static_assert!(lock_op::EX == 2 && lock_op::UN == 8 && lock_op::NB == 4);

/// Take an advisory lock on an open file, without blocking.
///
/// Returns `true` if the lock was taken. A `false` return is not an error to
/// escalate: an oversized log beats a blocked command, so the rotation path
/// skips rotating and appends anyway.
#[cfg(unix)]
pub fn try_lock_exclusive(file: &std::fs::File) -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: flock(2) takes an open descriptor and a flag word, dereferencing
    // no caller memory. `fd` is borrowed from `file`, which outlives the call,
    // and EX | NB is a valid operation pair. Failure is reported by return
    // value, not by corrupting anything.
    unsafe { flock(fd, lock_op::EX | lock_op::NB) == 0 }
}

/// Does this file's mode mark it executable?
///
/// Called only after the caller has confirmed `meta` belongs to a regular
/// file; this checks permissions, not file type.
#[cfg(unix)]
pub fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// See the Unix version of [`is_executable`] for the contract.
///
/// No verified target reaches this branch; permission bits don't carry the
/// same meaning off Unix, so a regular file is treated as executable once the
/// caller has already confirmed it is one.
#[cfg(not(unix))]
pub fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}

/// Release an advisory lock taken by [`try_lock_exclusive`].
///
/// Closing the file releases the lock too; this exists for the case where the
/// file stays open afterwards.
#[cfg(unix)]
pub fn unlock(file: &std::fs::File) {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: same contract as try_lock_exclusive. UN is a valid operation, and
    // unlocking a file we did not lock is a no-op rather than an error.
    unsafe {
        flock(fd, lock_op::UN);
    }
}

/// Where Lens keeps its state, resolved from the environment.
///
/// XDG variables win when they are set, on every platform — a user who exports
/// `XDG_CACHE_HOME` on macOS means it. Otherwise the fallback is native:
/// `~/.cache`, `~/.config`, `~/.local/state` on Linux, and the `~/Library`
/// equivalents on macOS.
///
/// `LENS_STORE` and `LENS_LOG_DIR` override the results entirely; that is what
/// lets the test suite run without touching a developer's real directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// Content-addressed run store lives under here.
    pub cache: PathBuf,
    /// Lens configuration lives under here.
    pub config: PathBuf,
    /// Logs live under here.
    pub state: PathBuf,
}

impl Dirs {
    /// Resolve from the process environment.
    pub fn from_env() -> Self {
        Self::resolve(|key| std::env::var_os(key), std::env::consts::OS)
    }

    /// Resolve from an injected environment.
    ///
    /// Both platform branches are exercised from any host this way, which is
    /// what keeps the macOS placeholder from being wholly untested.
    pub fn resolve<F>(env: F, os: &str) -> Self
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let home = env("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));

        let xdg = |var: &str, native: &str| -> PathBuf {
            match env(var) {
                // An exported-but-empty variable is a shell accident, not a
                // request to put the store at the filesystem root.
                Some(value) if !value.is_empty() => PathBuf::from(value),
                _ => home.join(native),
            }
        };

        if os == "macos" {
            Dirs {
                cache: xdg("XDG_CACHE_HOME", "Library/Caches"),
                config: xdg("XDG_CONFIG_HOME", "Library/Application Support"),
                state: xdg("XDG_STATE_HOME", "Library/Logs"),
            }
        } else {
            Dirs {
                cache: xdg("XDG_CACHE_HOME", ".cache"),
                config: xdg("XDG_CONFIG_HOME", ".config"),
                state: xdg("XDG_STATE_HOME", ".local/state"),
            }
        }
    }

    /// Directory holding stored runs, honoring `LENS_STORE`.
    pub fn store(&self, override_dir: Option<&OsString>) -> PathBuf {
        match override_dir {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => self.cache.join("lens").join("runs"),
        }
    }

    /// Directory holding logs, honoring `LENS_LOG_DIR`.
    pub fn logs(&self, override_dir: Option<&OsString>) -> PathBuf {
        match override_dir {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => self.state.join("lens").join("logs"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn is_executable_reads_the_permission_bits() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!("lens-exec-test-{}", std::process::id()));
        std::fs::write(&path, b"#!/bin/sh\n").expect("write file");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&std::fs::metadata(&path).unwrap()));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&std::fs::metadata(&path).unwrap()));

        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn set_arg0_changes_only_argv0() {
        // A child that echoes its own argv[0] is the only way to observe this
        // from outside; /bin/sh -c is the smallest one available everywhere.
        let mut cmd = Command::new("/bin/sh");
        set_arg0(&mut cmd, "custom-name");
        cmd.arg("-c").arg("echo \"$0\"");
        let output = cmd.output().expect("run shell");
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "custom-name");
    }

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let owned: Vec<(String, String)> =
            pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| OsString::from(v))
    }

    #[test]
    fn signal_deaths_follow_the_shell_convention() {
        assert_eq!(exit_code_for_signal(15), 143); // SIGTERM
        assert_eq!(exit_code_for_signal(9), 137); // SIGKILL
        assert_eq!(exit_code_for_signal(2), 130); // SIGINT
    }

    #[test]
    fn stdin_tty_detection_answers_without_panicking() {
        // The value depends on how the test binary was invoked, so assert only
        // that the call is safe to make. `is_tty` is called on every run.
        let _ = is_tty(STDIN_FD);
    }

    #[test]
    fn linux_falls_back_to_xdg_defaults() {
        let dirs = Dirs::resolve(env_from(&[("HOME", "/home/u")]), "linux");
        assert_eq!(dirs.cache, PathBuf::from("/home/u/.cache"));
        assert_eq!(dirs.config, PathBuf::from("/home/u/.config"));
        assert_eq!(dirs.state, PathBuf::from("/home/u/.local/state"));
    }

    #[test]
    fn macos_falls_back_to_library() {
        let dirs = Dirs::resolve(env_from(&[("HOME", "/Users/u")]), "macos");
        assert_eq!(dirs.cache, PathBuf::from("/Users/u/Library/Caches"));
        assert_eq!(dirs.config, PathBuf::from("/Users/u/Library/Application Support"));
        assert_eq!(dirs.state, PathBuf::from("/Users/u/Library/Logs"));
    }

    #[test]
    fn xdg_vars_win_on_both_platforms() {
        let env = env_from(&[
            ("HOME", "/Users/u"),
            ("XDG_CACHE_HOME", "/tmp/c"),
            ("XDG_CONFIG_HOME", "/tmp/g"),
            ("XDG_STATE_HOME", "/tmp/s"),
        ]);
        for os in ["linux", "macos"] {
            let dirs = Dirs::resolve(&env, os);
            assert_eq!(dirs.cache, PathBuf::from("/tmp/c"), "{os}");
            assert_eq!(dirs.config, PathBuf::from("/tmp/g"), "{os}");
            assert_eq!(dirs.state, PathBuf::from("/tmp/s"), "{os}");
        }
    }

    #[test]
    fn empty_xdg_var_is_treated_as_unset() {
        let dirs = Dirs::resolve(env_from(&[("HOME", "/home/u"), ("XDG_CACHE_HOME", "")]), "linux");
        assert_eq!(dirs.cache, PathBuf::from("/home/u/.cache"));
    }

    #[test]
    fn store_and_log_paths_are_overridable() {
        let dirs = Dirs::resolve(env_from(&[("HOME", "/home/u")]), "linux");
        assert_eq!(dirs.store(None), PathBuf::from("/home/u/.cache/lens/runs"));
        assert_eq!(dirs.logs(None), PathBuf::from("/home/u/.local/state/lens/logs"));

        let over = OsString::from("/tmp/isolated");
        assert_eq!(dirs.store(Some(&over)), PathBuf::from("/tmp/isolated"));
        assert_eq!(dirs.logs(Some(&over)), PathBuf::from("/tmp/isolated"));
    }

    #[test]
    fn missing_home_does_not_panic() {
        let dirs = Dirs::resolve(env_from(&[]), "linux");
        assert_eq!(dirs.cache, PathBuf::from("/.cache"));
    }

    #[test]
    fn an_advisory_lock_excludes_a_second_holder() {
        // The property rotation depends on: while one process holds the lock,
        // another must be told no rather than made to wait.
        let path = std::env::temp_dir().join(format!("lens-lock-{}", std::process::id()));
        let first = std::fs::File::create(&path).expect("create lock file");
        let second = std::fs::File::open(&path).expect("open lock file again");

        assert!(try_lock_exclusive(&first), "first holder takes the lock");
        assert!(!try_lock_exclusive(&second), "second holder is refused, not blocked");

        unlock(&first);
        assert!(try_lock_exclusive(&second), "lock is available once released");

        drop(first);
        drop(second);
        let _ = std::fs::remove_file(&path);
    }
}
