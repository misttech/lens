// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Lens runs a real command, keeps its full output, and shows an AI coding agent
//! the view of that output worth spending context on.
//!
//! Lens runs the command, writes every byte it produced to a content-addressed
//! store, and emits them. No filtering happens yet — what exists is the part
//! that has to be right before anything is allowed to remove content: the
//! child's exit code, its bytes, its terminal, a record of every invocation,
//! and a handle that can re-derive any view of the run later.

mod cli;
mod executor;
mod log;
mod platform;
mod resolve;
mod static_assert;
mod store;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use crate::cli::{Invocation, Subcommand};
use crate::log::{Level, Logger, RunRecord};
use crate::resolve::{PassthroughReason, Plan};
use crate::store::Store;

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
        Invocation::Subcommand { name: Subcommand::Show, args } => show(&args),
        Invocation::Subcommand { name: Subcommand::Stats, args } => stats(&args),
        Invocation::Subcommand { name: Subcommand::Logs, args } => logs(&args),
        Invocation::Subcommand { name, .. } => {
            // Naming what it waits on is more useful than "unimplemented": it
            // says this is a gap in the build, not a missing feature.
            fail(&format!("`lens {name}` is not implemented yet — it arrives with the pipeline"))
        }
        Invocation::Run { argv } => run(&argv),
    }
}

/// Where this invocation reads and writes its state.
///
/// The three `LENS_*` overrides exist so the test suite can run against a temp
/// directory: no test may touch a developer's real cache, config or logs.
struct Env {
    store: PathBuf,
    logs: PathBuf,
    log_level: Level,
}

impl Env {
    fn from_process() -> Self {
        let dirs = platform::Dirs::from_env();
        let store_override = std::env::var_os("LENS_STORE");
        let logs_override = std::env::var_os("LENS_LOG_DIR");
        Env {
            store: dirs.store(store_override.as_ref()),
            logs: dirs.logs(logs_override.as_ref()),
            log_level: log_level_from_env(),
        }
    }

    fn logger(&self) -> Logger {
        let mut config = log::Config::new(&self.logs);
        config.level = self.log_level;
        Logger::init(&config)
    }
}

/// `LENS_LOG`, defaulting to info. An unparseable value falls back to the
/// default rather than failing the run: a typo in a log level is not a reason
/// to refuse to run a command.
fn log_level_from_env() -> Level {
    std::env::var_os("LENS_LOG")
        .and_then(|value| Level::parse(&value.to_string_lossy()))
        .unwrap_or_default()
}

/// Run a command: passthrough or capture, then propagate its fate.
fn run(argv: &[String]) -> ExitCode {
    let env = Env::from_process();
    let mode_raw =
        std::env::var_os("LENS_MODE").is_some_and(|mode| mode.eq_ignore_ascii_case("raw"));
    let stdin_is_tty = platform::is_tty(platform::STDIN_FD);
    let lens_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    let cwd = std::env::current_dir().unwrap_or_default();

    let plan = resolve::plan(argv, mode_raw, stdin_is_tty, lens_exe.as_deref(), path_var.as_ref());

    match plan {
        Plan::Passthrough { reason } => {
            // The record has to be written before the exec, because after it
            // there is no process left to write anything.
            env.logger().run(passthrough_record(argv, &cwd, reason));
            passthrough(argv)
        }
        Plan::Capture { program } => match executor::capture(&program, argv) {
            Ok(captured) => {
                let duration_ms = captured.duration.as_millis() as u64;
                // Store first, emit second: the handle belongs in the output,
                // and from M3 the elision marker that carries it is the only
                // way a reader learns the rest of the run is still available.
                let store = Store::new(&env.store);
                let handle = store
                    .write(
                        argv,
                        &cwd,
                        &captured.stdout,
                        &captured.stderr,
                        captured.exit_code,
                        duration_ms,
                    )
                    .ok();

                let logger = env.logger();
                if handle.is_none() {
                    // Invariant 5 is about not losing output, and a store that
                    // could not be written has lost the ability to re-derive
                    // this run. That is worth a warning, and worth nothing more:
                    // the command still succeeds and its output still lands.
                    logger.event(Level::Warn, "store write failed", &[("cmd", command_name(argv))]);
                }
                logger.run(RunRecord {
                    handle: handle.map(|h| h.to_string()),
                    cmd: command_name(argv).to_string(),
                    argv: argv.to_vec(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    exit: Some(captured.exit_code),
                    dur_ms: Some(duration_ms),
                    out_bytes: Some(captured.stdout.len() as u64),
                    err_bytes: Some(captured.stderr.len() as u64),
                    passthrough: false,
                    reason: None,
                });

                emit(&captured.stdout, &captured.stderr);
                exit_with(captured.exit_code)
            }
            // Invariant 6: an internal failure is never allowed to become the
            // user's failure. If capture could not start, run the command the
            // ordinary way and let it speak for itself.
            Err(err) => {
                let logger = env.logger();
                logger.event(
                    Level::Warn,
                    "capture failed, falling back to passthrough",
                    &[("cmd", command_name(argv)), ("err", &err.to_string())],
                );
                logger.run(passthrough_record(argv, &cwd, PassthroughReason::CaptureFailed));
                passthrough(argv)
            }
        },
    }
}

/// The record for a run whose outcome this process will never see.
fn passthrough_record(
    argv: &[String],
    cwd: &std::path::Path,
    reason: PassthroughReason,
) -> RunRecord {
    RunRecord {
        handle: None,
        cmd: command_name(argv).to_string(),
        argv: argv.to_vec(),
        cwd: cwd.to_string_lossy().into_owned(),
        exit: None,
        dur_ms: None,
        out_bytes: None,
        err_bytes: None,
        passthrough: true,
        reason: Some(reason.as_str().to_string()),
    }
}

/// The command name without its directory, as `lens stats` groups by.
fn command_name(argv: &[String]) -> &str {
    argv[0].rsplit('/').next().unwrap_or(&argv[0])
}

/// `lens show <handle>`.
///
/// Re-emits a stored run without re-executing the command — which is the whole
/// point of the store, and the half of it a user can observe today. `--level N`
/// arrives with the renderer; until then every view is the raw one, which is
/// level 3.
fn show(args: &[String]) -> ExitCode {
    let env = Env::from_process();

    let Some(text) = args.first() else {
        return fail(&"lens show needs a handle, e.g. lens show a3f19c2b");
    };
    if let Some(flag) = args.iter().find(|a| a.starts_with("--")) {
        return fail(&format!("`{flag}` is not implemented yet — it arrives with the renderer"));
    }
    let Some(handle) = store::Handle::parse(text) else {
        return fail(&format!("`{text}` is not a handle — expected 8 hex digits"));
    };

    let store = Store::new(&env.store);
    let Ok(stdout) = store.read_stream(&handle, store::Stream::Stdout) else {
        return fail(&format!("no run `{handle}` in {}", env.store.display()));
    };
    let stderr = store.read_stream(&handle, store::Stream::Stderr).unwrap_or_default();

    emit(&stdout, &stderr);

    // The stored run's exit code is reported by `lens show`'s own exit code, so
    // a script re-reading a run sees what the command did. Reading a run is not
    // running it, but reporting success for a failed run would be a lie.
    match store.read_meta(&handle) {
        Ok(meta) => exit_with(meta.exit_code),
        Err(_) => ExitCode::SUCCESS,
    }
}

/// `lens stats [--since 7d] [--cmd git]`.
fn stats(args: &[String]) -> ExitCode {
    let env = Env::from_process();

    let mut since = None;
    let mut cmd = None;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--since" => match rest.next() {
                Some(spec) => match log::since_cutoff(spec, std::time::SystemTime::now()) {
                    Some(cutoff) => since = Some(cutoff),
                    None => {
                        return fail(&format!("unrecognized duration `{spec}` — try 7d, 24h, 30m"));
                    }
                },
                None => return fail(&"--since needs a duration, e.g. 7d"),
            },
            "--cmd" => match rest.next() {
                Some(name) => cmd = Some(name.clone()),
                None => return fail(&"--cmd needs a command name"),
            },
            other => return fail(&format!("unknown flag `{other}` for lens stats")),
        }
    }

    let records = log::read_all(&env.logs, log::DEFAULT_MAX_FILES);
    let stats = log::aggregate(&records, since.as_deref(), cmd.as_deref());

    if stats.runs == 0 {
        println!("no runs recorded in {}", env.logs.display());
        return ExitCode::SUCCESS;
    }

    let pct = |part: u64| (part as f64 / stats.runs as f64) * 100.0;
    println!("runs              {:>10}", stats.runs);
    println!("passthrough       {:>10}  ({:.0}%)", stats.passthrough, pct(stats.passthrough));
    println!("captured bytes    {:>10}", stats.bytes);

    if !stats.by_command.is_empty() {
        println!("\nby command");
        let mut rows: Vec<(&String, &u64)> = stats.by_command.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (name, count) in rows {
            println!("  {name:<14}{count:>8}");
        }
    }

    if !stats.reasons.is_empty() {
        println!("\npassthrough reasons");
        let mut rows: Vec<(&String, &u64)> = stats.reasons.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (reason, count) in rows {
            println!("  {reason:<22}{count:>8}");
        }
    }

    ExitCode::SUCCESS
}

/// `lens logs [--tail N] [--level warn]`.
fn logs(args: &[String]) -> ExitCode {
    let env = Env::from_process();

    let mut tail = 20usize;
    let mut level = None;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--tail" => match rest.next().and_then(|n| n.parse().ok()) {
                Some(n) => tail = n,
                None => return fail(&"--tail needs a count"),
            },
            "--level" => match rest.next().and_then(|name| Level::parse(name)) {
                Some(parsed) => level = Some(parsed),
                None => return fail(&"--level needs one of: off error warn info debug trace"),
            },
            other => return fail(&format!("unknown flag `{other}` for lens logs")),
        }
    }

    let records = log::read_all(&env.logs, log::DEFAULT_MAX_FILES);
    let matching: Vec<&log::Record> =
        records.iter().filter(|r| level.is_none_or(|want| r.lvl <= want)).collect();

    for record in matching.iter().rev().take(tail).rev() {
        match serde_json::to_string(record) {
            Ok(line) => println!("{line}"),
            Err(_) => continue,
        }
    }
    ExitCode::SUCCESS
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
  lens show <handle>            re-emit a stored run without re-running it
  lens stats [--since 7d] [--cmd git]
  lens logs [--tail N] [--level warn]

lens flags go before the command name. Everything after it belongs to the
child, including tokens that look like lens flags.

environment:
  LENS_MODE=raw                 emit raw output, exit with the child's code
  LENS_LOG=<level>              off error warn info debug trace (default info)
  LENS_STORE=<dir>              where runs are kept
  LENS_LOG_DIR=<dir>            where logs are kept

not implemented yet: explain, plot, lenses, config
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
