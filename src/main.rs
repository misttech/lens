// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Lens runs a real command, keeps its full output, and shows an AI coding agent
//! the view of that output worth spending context on.
//!
//! This milestone is the fidelity core: Lens runs the command, captures both
//! streams, and emits them unchanged. No filtering happens yet, which makes it
//! the right time to nail down the properties every later stage depends on —
//! the child's exit code, its bytes, and its terminal.

mod cli;
mod executor;
mod platform;
mod resolve;
mod static_assert;

use std::io::Write;
use std::process::{Command, ExitCode};

use crate::cli::Invocation;
use crate::resolve::Plan;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let invocation = match cli::parse(args) {
        Ok(invocation) => invocation,
        Err(err) => return fail(&err),
    };

    match invocation {
        Invocation::Help => {
            print!("{}", help_text());
            ExitCode::SUCCESS
        }
        Invocation::Version => {
            println!("lens {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Invocation::Subcommand { name, .. } => {
            // Naming the milestone is more useful than "unimplemented": it tells
            // the reader this is a gap in the build, not a missing feature.
            fail(&format!(
                "`lens {name}` is not implemented yet — it arrives with the output store"
            ))
        }
        Invocation::Run { argv } => run(&argv),
    }
}

/// Run a command: passthrough or capture, then propagate its fate.
fn run(argv: &[String]) -> ExitCode {
    let mode_raw =
        std::env::var_os("LENS_MODE").is_some_and(|mode| mode.eq_ignore_ascii_case("raw"));
    let stdin_is_tty = platform::is_tty(platform::STDIN_FD);
    let lens_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");

    let plan = resolve::plan(argv, mode_raw, stdin_is_tty, lens_exe.as_deref(), path_var.as_ref());

    match plan {
        Plan::Passthrough { .. } => passthrough(argv),
        Plan::Capture { program } => match executor::capture(&program, argv) {
            Ok(captured) => {
                emit(&captured.stdout, &captured.stderr);
                exit_with(captured.exit_code)
            }
            // Invariant 6: an internal failure is never allowed to become the
            // user's failure. If capture could not start, run the command the
            // ordinary way and let it speak for itself.
            Err(_) => passthrough(argv),
        },
    }
}

/// Replace this process with the child.
///
/// Byte-identical to running the command directly, because after this point the
/// command *is* the process.
fn passthrough(argv: &[String]) -> ExitCode {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);

    #[cfg(unix)]
    {
        let err = platform::exec(&mut cmd);
        // exec only returns on failure. A command that could not be started is
        // the shell's classic 127.
        eprintln!("lens: {}: {err}", argv[0]);
        ExitCode::from(127)
    }

    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(status) => exit_with(platform::exit_code_for_status(&status)),
            Err(err) => {
                eprintln!("lens: {}: {err}", argv[0]);
                ExitCode::from(127)
            }
        }
    }
}

/// Write the child's streams out, separately and in full.
fn emit(stdout: &[u8], stderr: &[u8]) {
    // Written as bytes, never as text: the output may not be UTF-8, and level 3
    // has to diff clean against what the command actually produced.
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(stdout);
    let _ = out.flush();

    let mut err = std::io::stderr().lock();
    let _ = err.write_all(stderr);
    let _ = err.flush();
}

/// Turn a child's exit code into this process's.
///
/// `ExitCode` carries a `u8`, which is what `wait(2)` reports anyway; a code
/// outside that range came from a platform that does not exist here, and 1 is
/// the honest answer for "it failed, in a way I cannot name".
fn exit_with(code: i32) -> ExitCode {
    ExitCode::from(exit_code_u8(code))
}

/// The `u8` an exit code narrows to, or 1 if it does not fit.
fn exit_code_u8(code: i32) -> u8 {
    u8::try_from(code).unwrap_or(1)
}

/// Report a Lens problem — never a child's problem — and exit non-zero.
fn fail(err: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("lens: {err}");
    ExitCode::FAILURE
}

fn help_text() -> String {
    format!(
        "lens {version} — run a command, keep its full output, show the view worth reading

usage:
  lens <command> [args...]      run a command and filter its output
  lens --version
  lens --help

lens flags go before the command name. Everything after it belongs to the
child, including tokens that look like lens flags.

environment:
  LENS_MODE=raw                 emit raw output, exit with the child's code

not implemented yet: show, explain, stats, plot, lenses, config, logs
",
        version = env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_round_trip_through_the_u8_range() {
        // The codes that matter: success, generic failure, and a signal death
        // reported the way a shell reports it.
        for code in [0, 1, 3, 42, 127, 143, 255] {
            assert_eq!(exit_code_u8(code), code as u8);
        }
    }

    #[test]
    fn an_out_of_range_code_is_still_a_failure() {
        // Never silently becomes 0: a failing command that looks successful is
        // the worst answer this function can give.
        assert_eq!(exit_code_u8(4096), 1);
        assert_eq!(exit_code_u8(-1), 1);
    }

    #[test]
    fn help_names_the_split_rule() {
        // The one thing a user has to know to type a correct lens command.
        let help = help_text();
        assert!(help.contains("before the command name"));
        assert!(help.contains("LENS_MODE=raw"));
    }
}
