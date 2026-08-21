// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The OS boundary.
//!
//! Every `#[cfg]` and every `unsafe` block in this tree lives here, so a port is
//! a matter of implementing this file rather than auditing the whole crate.
//!
//! Linux is the verified target. macOS compiles and its branches are written,
//! but nothing claims it works until someone runs the suite on a Mac. What the
//! two share — `exec`, `isatty`, the signal-to-exit-code mapping — is identical,
//! so the only real divergence is where user directories live, which arrives
//! with the store that needs them.

use std::process::Command;

use crate::static_assert;

/// Exit code a shell reports for a child killed by `signum`.
///
/// `LENS.md` invariant 2: the child's fate is reported unchanged, and a signal
/// death is reported the way every shell reports it.
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
/// This is how passthrough keeps invariant 6 honest. Spawning and copying
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

/// Is `fd` a terminal?
///
/// Used to decide whether a child may take over the terminal (`LENS.md` §4). A
/// wrong answer here is expensive in one direction only: capturing an
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

// The one language boundary in this crate: libc calls declared in-tree rather
// than pulling in a crate for them. Today that is `isatty` for interactive
// detection (§4); the log's `flock` joins it when rotation exists.
//
// `c_int` is the ABI contract for both. Asserting its width at compile time is
// cheap and catches a target where the assumption silently stops holding.
static_assert!(core::mem::size_of::<std::os::raw::c_int>() == 4);
static_assert!(core::mem::align_of::<std::os::raw::c_int>() == 4);

#[cfg(unix)]
unsafe extern "C" {
    fn isatty(fd: std::os::raw::c_int) -> std::os::raw::c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
