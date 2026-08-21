// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Running the child and capturing what it produced.
//!
//! Three properties this module owes the rest of Lens, each one an invariant
//! rather than a preference:
//!
//! * **Exit code fidelity.** The child's code is propagated
//!   unchanged, and a signal death becomes `128 + signum`.
//! * **Stream separation.** stdout and stderr are captured and
//!   emitted separately, never merged. The cost is that their interleaving is
//!   lost relative to a terminal run; the benefit is that a stage can reason
//!   about which stream a line came from, which is what lets a failing command's
//!   stderr be force-kept.
//! * **No inherited configuration.** Every `LENS_*` variable is removed
//!   from the child's environment, so a nested `lens` — which should not exist,
//!   but scripts surprise you — cannot inherit a budget.

use std::ffi::OsString;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Everything a completed child produced.
#[derive(Debug, Clone)]
pub struct Captured {
    /// Raw stdout bytes, exactly as written.
    pub stdout: Vec<u8>,
    /// Raw stderr bytes, exactly as written.
    pub stderr: Vec<u8>,
    /// The code Lens should exit with (`128 + signum` for a signal death).
    pub exit_code: i32,
    /// Wall time the child took. Lens's own overhead is measured separately.
    pub duration: Duration,
}

/// Run `argv` with `program` as the resolved binary, capturing both streams.
///
/// # Errors
///
/// Returns the spawn error if the child could not be started. Callers treat
/// that as a reason to fall back to passthrough rather than as a
/// failure of the user's command.
pub fn capture(program: &std::path::Path, argv: &[String]) -> std::io::Result<Captured> {
    debug_assert!(!argv.is_empty(), "cli::parse guarantees a non-empty argv");

    let mut cmd = Command::new(program);
    // argv[0] is the name the child sees; keep it as the user typed it rather
    // than as we resolved it, so a command that inspects its own name behaves
    // the way it would outside Lens.
    cmd.arg0(&argv[0]);
    cmd.args(child_args(argv));
    scrub_env(&mut cmd);
    quiet_git(&mut cmd, argv);

    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let started = Instant::now();
    let mut child = cmd.spawn()?;

    // Both pipes must be drained concurrently. A child that fills the stderr
    // pipe while we are still reading stdout blocks forever, and "forever" is
    // the one runtime Lens cannot afford — so one thread per stream, which is
    // also why there is no async runtime in the dependency list.
    let mut stdout_pipe = child.stdout.take().expect("stdout piped above");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped above");

    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let status = child.wait()?;
    // A panicked reader thread means we lost that stream, not that the run
    // failed: report what we have rather than losing the child's exit code.
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    Ok(Captured {
        stdout,
        stderr,
        exit_code: crate::platform::exit_code_for_status(&status),
        duration: started.elapsed(),
    })
}

/// The arguments after the command name, with any Lens-required additions.
fn child_args(argv: &[String]) -> Vec<String> {
    let mut args: Vec<String> = argv[1..].to_vec();
    if basename(&argv[0]) == "git" {
        // A pager would swallow the output we are here to read. `--no-pager` is
        // a global git flag, so it is valid in front of any subcommand and any
        // other global flag.
        args.insert(0, "--no-pager".to_string());
    }
    args
}

/// Remove every `LENS_*` variable from the child's environment.
fn scrub_env(cmd: &mut Command) {
    scrub_env_from(cmd, std::env::vars_os().map(|(key, _)| key));
}

/// The same, over an injected variable list, so the rule can be tested without
/// mutating the test process's own environment.
fn scrub_env_from<I>(cmd: &mut Command, vars: I)
where
    I: IntoIterator<Item = OsString>,
{
    for key in vars {
        if key.as_encoded_bytes().starts_with(b"LENS_") {
            cmd.env_remove(&key);
        }
    }
}

/// Make git produce plain, unpaged, uncolored output when captured.
fn quiet_git(cmd: &mut Command, argv: &[String]) {
    if basename(&argv[0]) != "git" {
        return;
    }
    cmd.env("GIT_PAGER", "cat");
    cmd.env("TERM", "dumb");
}

/// The command name without its directory.
fn basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    fn sh(script: &str) -> Captured {
        capture(Path::new("/bin/sh"), &argv(&["sh", "-c", script])).expect("spawn /bin/sh")
    }

    #[test]
    fn streams_are_captured_separately() {
        let out = sh("echo to-stdout; echo to-stderr >&2");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "to-stdout\n");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "to-stderr\n");
    }

    #[test]
    fn exit_codes_are_propagated_unchanged() {
        assert_eq!(sh("exit 0").exit_code, 0);
        assert_eq!(sh("exit 1").exit_code, 1);
        assert_eq!(sh("exit 3").exit_code, 3);
        assert_eq!(sh("exit 42").exit_code, 42);
    }

    #[test]
    fn signal_deaths_become_128_plus_signum() {
        // The shell reports its own child's signal death as 128+n and exits
        // with that, so this asserts the arithmetic end to end.
        assert_eq!(sh("kill -TERM $$").exit_code, 143);
    }

    #[test]
    fn large_output_on_both_streams_does_not_deadlock() {
        // The regression this guards: draining one pipe at a time blocks
        // forever once the other fills. 256 KiB is well past a pipe buffer.
        let out = sh("yes stdout-line | head -c 262144; yes stderr-line | head -c 262144 >&2");
        assert_eq!(out.stdout.len(), 262_144);
        assert_eq!(out.stderr.len(), 262_144);
    }

    #[test]
    fn lens_variables_are_removed_and_others_are_not() {
        // Injected rather than set globally: the test process's environment is
        // shared by every test thread, and this rule is worth testing without
        // that hazard. The end-to-end check lives in tests/fidelity.rs, where
        // the variable can be set on the child alone.
        let mut cmd = Command::new("/bin/true");
        let vars = ["LENS_BUDGET", "LENS_MODE", "LENS_DEBUG", "LENSED", "PATH", "HOME"]
            .into_iter()
            .map(OsString::from);
        scrub_env_from(&mut cmd, vars);

        // get_envs yields its own order, so compare as a sorted set.
        let mut removed: Vec<String> = cmd
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        removed.sort();
        assert_eq!(removed, vec!["LENS_BUDGET", "LENS_DEBUG", "LENS_MODE"]);
        // `LENSED` does not carry the underscore, so it is somebody else's
        // variable and stays.
        assert!(!removed.iter().any(|k| k == "LENSED"));
    }

    #[test]
    fn git_gets_no_pager_and_a_dumb_terminal() {
        let args = child_args(&argv(&["git", "diff", "--stat"]));
        assert_eq!(args, vec!["--no-pager", "diff", "--stat"]);

        // The flag goes in front of git's own global flags, where git accepts it.
        let args = child_args(&argv(&["git", "-C", "/tmp", "status"]));
        assert_eq!(args, vec!["--no-pager", "-C", "/tmp", "status"]);
    }

    #[test]
    fn other_commands_are_not_rewritten() {
        assert_eq!(child_args(&argv(&["cargo", "test"])), vec!["test".to_string()]);
        assert_eq!(child_args(&argv(&["ls"])), Vec::<String>::new());
    }

    #[test]
    fn the_child_is_timed() {
        // The duration lands in the run record and in meta.json, where it is
        // the child's time, not Lens's overhead.
        assert!(sh("exit 0").duration.as_nanos() > 0);
    }

    #[test]
    fn binary_output_survives_capture_unchanged() {
        // Level 3 has to be byte-identical to what the command produced, which
        // means the capture path cannot assume UTF-8 anywhere.
        let out = sh("printf '\\001\\002\\377\\000end'");
        assert_eq!(out.stdout, vec![0x01, 0x02, 0xff, 0x00, b'e', b'n', b'd']);
    }
}
