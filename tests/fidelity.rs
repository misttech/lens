// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! End-to-end checks on the properties that are non-negotiable.
//!
//! These run the real binary against real children. Everything here is a
//! property of the *tool*, not of a function, and each one is a bug rather than
//! a tradeoff if it fails.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The `lens` binary built alongside this test.
fn lens_bin() -> PathBuf {
    // The test executable lives in <target>/debug/deps/; the binary under test
    // is two directories up.
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("lens")
}

/// Run `lens <args>` and collect everything it produced.
fn lens(args: &[&str]) -> Output {
    Command::new(lens_bin()).args(args).output().expect("run lens")
}

/// Run the same command without Lens, for comparison.
fn bare(args: &[&str]) -> Output {
    Command::new(args[0]).args(&args[1..]).output().expect("run command directly")
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            128 + output.status.signal().expect("exited by code or signal")
        }
        #[cfg(not(unix))]
        {
            panic!("no exit code")
        }
    })
}

#[test]
fn exit_codes_are_propagated_unchanged() {
    // A wrong code here is worse than a wrong view: it tells the
    // caller a failed command succeeded.
    for expected in [0, 1, 3, 42, 127] {
        let out = lens(&["sh", "-c", &format!("exit {expected}")]);
        assert_eq!(code(&out), expected, "exit {expected}");
    }
}

#[test]
fn signal_deaths_follow_the_shell_convention() {
    // 128 + signum.
    let out = lens(&["sh", "-c", "kill -TERM $$"]);
    assert_eq!(code(&out), 143);
}

#[test]
fn streams_are_kept_separate() {
    // Merging them would be convenient and would destroy the
    // signal that later lets a failing command's stderr be force-kept.
    let out = lens(&["sh", "-c", "echo out; echo err >&2"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
}

#[test]
fn output_is_byte_identical_to_running_the_command_directly() {
    // Nothing filters yet, so this is the strongest form of the claim. Once
    // stages exist it narrows to LENS_MODE=raw and level 3, but the comparison
    // stays the same one.
    let script = "printf 'a\\nb\\n'; printf 'e1\\ne2\\n' >&2";
    let filtered = lens(&["sh", "-c", script]);
    let direct = bare(&["sh", "-c", script]);
    assert_eq!(filtered.stdout, direct.stdout);
    assert_eq!(filtered.stderr, direct.stderr);
    assert_eq!(code(&filtered), code(&direct));
}

#[test]
fn raw_mode_is_byte_identical_to_running_the_command_directly() {
    // The passthrough property. LENS_MODE=raw execs the child, so this
    // compares a replaced process image against the real thing.
    let script = "printf 'x\\ny\\n'; printf 'z\\n' >&2; exit 7";
    let filtered = Command::new(lens_bin())
        .args(["sh", "-c", script])
        .env("LENS_MODE", "raw")
        .output()
        .expect("run lens");
    let direct = bare(&["sh", "-c", script]);
    assert_eq!(filtered.stdout, direct.stdout);
    assert_eq!(filtered.stderr, direct.stderr);
    assert_eq!(code(&filtered), 7);
}

#[test]
fn binary_output_survives_unchanged() {
    // Output is bytes, not text. A stream that is not valid UTF-8 is passed
    // through unfiltered rather than mangled into replacement characters —
    // filtering it would mean guessing what its bytes mean.
    let script = r"printf '\001\002\377\000end'";
    let filtered = lens(&["sh", "-c", script]);
    let direct = bare(&["sh", "-c", script]);
    assert_eq!(filtered.stdout, direct.stdout);
    assert_eq!(filtered.stdout, vec![0x01, 0x02, 0xff, 0x00, b'e', b'n', b'd']);
}

#[test]
fn lens_variables_do_not_reach_the_child() {
    // A nested lens must not inherit a budget, and a child that inspects
    // its environment must not see Lens's configuration at all.
    let out = Command::new(lens_bin())
        .args(["sh", "-c", "env | grep -c '^LENS_' || true"])
        .env("LENS_BUDGET", "500")
        .env("LENS_DEBUG", "1")
        .output()
        .expect("run lens");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0");
}

#[test]
fn a_missing_command_fails_the_way_a_shell_fails() {
    // Lens does not editorialize about a command it could not
    // find. The child's failure is reported, not replaced.
    let out = lens(&["definitely-not-a-real-command-xyz"]);
    assert_ne!(code(&out), 0);
    assert!(out.stdout.is_empty(), "nothing is invented on stdout");
}

#[test]
fn an_unknown_lens_flag_does_not_become_a_command() {
    // Silently treating it as a command would try to execute `--not-a-flag`.
    let out = lens(&["--not-a-flag", "true"]);
    assert_ne!(code(&out), 0);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown lens flag"), "{stderr}");
    assert!(stderr.starts_with("lens:"), "a lens error says it is from lens: {stderr}");
}

#[test]
fn a_child_flag_that_looks_like_ours_reaches_the_child() {
    // The split rule, end to end: `--version` after the command name is the
    // child's, so this must print the shell's answer and not Lens's.
    let out = lens(&["sh", "-c", "echo child-saw:$1", "sh", "--version"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "child-saw:--version");
}

#[test]
fn version_and_help_answer_without_running_anything() {
    let out = lens(&["--version"]);
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("lens "));

    let out = lens(&["--help"]);
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("usage:"));
}

#[test]
fn large_interleaved_output_completes() {
    // The deadlock this guards is a hang, so a failure here shows up as a test
    // that never finishes rather than one that fails. Raw mode keeps it a test
    // of the capture path: filtering would collapse 100,000 identical lines to
    // one, which is correct and would hide what this is checking.
    let out = Command::new(lens_bin())
        .args(["sh", "-c", "yes a | head -c 200000; yes b | head -c 200000 >&2"])
        .env("LENS_MODE", "raw")
        .output()
        .expect("run lens");
    assert_eq!(out.stdout.len(), 200_000);
    assert_eq!(out.stderr.len(), 200_000);
    assert_eq!(out.status.code(), Some(0));
}
