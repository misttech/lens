// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The `lens` binary: argument handling, process control, and the decisions
//! that need an environment. Everything it filters with lives in the library.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use lens::cli::{Invocation, Subcommand};
use lens::log::{Level, Logger, RunRecord};
use lens::pipeline::{Ctx, Stream};
use lens::render::Level as ViewLevel;
use lens::resolve::{PassthroughReason, Plan};
use lens::store::Store;
use lens::tokens::{Heuristic, TokenEstimator};

fn main() -> ExitCode {
    let raw_args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    // `cli::parse` needs every token as `String`, and `std::env::args()` gets
    // there by panicking on the first argument that is not valid UTF-8 — a
    // Lens failure standing in for the user's command. `cli::parse` only ever
    // makes a Lens-level decision from the first token (every later token is
    // either a subcommand's or the child's, and reaches it verbatim), so a
    // non-UTF-8 byte anywhere is doubt about how to interpret this line, not
    // doubt about whether to run it. Invariant 6 resolves that toward running
    // it exactly as given.
    let Some(args): Option<Vec<String>> =
        raw_args.iter().map(|a| a.to_str().map(str::to_string)).collect()
    else {
        return exec_raw(&raw_args);
    };

    let invocation = match lens::cli::parse(args) {
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
        let dirs = lens::platform::Dirs::from_env();
        let store_override = std::env::var_os("LENS_STORE");
        let logs_override = std::env::var_os("LENS_LOG_DIR");
        Env {
            store: dirs.store(store_override.as_ref()),
            logs: dirs.logs(logs_override.as_ref()),
            log_level: log_level_from_env(),
        }
    }

    fn logger(&self) -> Logger {
        let mut config = lens::log::Config::new(&self.logs);
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
    let stdin_is_tty = lens::platform::is_tty(lens::platform::STDIN_FD);
    let lens_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    let cwd = std::env::current_dir().unwrap_or_default();

    let plan =
        lens::resolve::plan(argv, mode_raw, stdin_is_tty, lens_exe.as_deref(), path_var.as_ref());

    match plan {
        Plan::Passthrough { reason } => {
            // The record has to be written before the exec, because after it
            // there is no process left to write anything.
            env.logger().run(passthrough_record(argv, &cwd, reason));
            passthrough(argv)
        }
        Plan::Capture { program } => match lens::executor::capture(&program, argv) {
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

                let handle_str = handle.as_ref().map(|h| h.to_string());
                let logger = env.logger();
                if handle.is_none() {
                    // A store that could not be written has lost the ability to
                    // re-derive this run. That is worth a warning and nothing
                    // more: the command still succeeds and its output lands.
                    logger.event(Level::Warn, "store write failed", &[("cmd", command_name(argv))]);
                }
                // Filter before logging, so the record can say what the caller
                // actually received rather than what was captured.
                let level = ViewLevel::from_number(
                    level_from_env().unwrap_or(lens::render::DEFAULT_LEVEL.number()),
                )
                .unwrap_or(lens::render::DEFAULT_LEVEL);
                let view = filter(
                    &captured.stdout,
                    &captured.stderr,
                    captured.exit_code,
                    level,
                    handle_str.as_deref(),
                );

                logger.run(RunRecord {
                    handle: handle_str,
                    cmd: command_name(argv).to_string(),
                    argv: argv.to_vec(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    exit: Some(captured.exit_code),
                    dur_ms: Some(duration_ms),
                    out_bytes: Some(captured.stdout.len() as u64),
                    err_bytes: Some(captured.stderr.len() as u64),
                    in_lines: Some(view.in_lines as u64),
                    out_lines: Some(view.out_lines as u64),
                    in_tok: Some(view.in_tok as u64),
                    out_tok: Some(view.out_tok as u64),
                    level: Some(level.number()),
                    stages: view.stages.clone(),
                    passthrough: false,
                    reason: None,
                });

                emit(&view.stdout, &view.stderr);
                exit_with(captured.exit_code)
            }
            // An internal failure is never allowed to become the user's
            // failure. If capture could not start, run the command the ordinary
            // way and let it speak for itself.
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
        in_lines: None,
        out_lines: None,
        in_tok: None,
        out_tok: None,
        level: None,
        stages: Vec::new(),
        passthrough: true,
        reason: Some(reason.as_str().to_string()),
    }
}

/// The command name without its directory, as `lens stats` groups by.
fn command_name(argv: &[String]) -> &str {
    argv[0].rsplit('/').next().unwrap_or(&argv[0])
}

/// `lens show <handle> [--level N]`.
///
/// Re-derives a view from stored bytes. The command is not re-executed, which is
/// the whole point: a reader who wants more detail pays for the render, not for
/// the run — and gets the same bytes however long ago it happened.
fn show(args: &[String]) -> ExitCode {
    let env = Env::from_process();

    let mut handle_text: Option<&String> = None;
    let mut level = ViewLevel::Raw;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--level" => {
                match rest.next().and_then(|n| n.parse().ok()).and_then(ViewLevel::from_number) {
                    Some(parsed) => level = parsed,
                    None => return fail(&"--level takes 0, 1, 2 or 3"),
                }
            }
            other if other.starts_with('-') => {
                return fail(&format!("unknown flag `{other}` for lens show"));
            }
            _ => handle_text = Some(arg),
        }
    }

    let Some(text) = handle_text else {
        return fail(&"lens show needs a handle, e.g. lens show a3f19c2b");
    };
    let Some(handle) = lens::store::Handle::parse(text) else {
        return fail(&format!("`{text}` is not a handle — expected 8 hex digits"));
    };

    let store = Store::new(&env.store);
    let Ok(stdout) = store.read_stream(&handle, lens::store::Stream::Stdout) else {
        return fail(&format!("no run `{handle}` in {}", env.store.display()));
    };
    let stderr = store.read_stream(&handle, lens::store::Stream::Stderr).unwrap_or_default();
    let meta = store.read_meta(&handle).ok();
    // `meta.json` can be missing from a run interrupted mid-write (store.rs
    // documents that a partial entry is possible). Defaulting to 0 there would
    // report success for a run whose fate is unrecorded — the exact lie this
    // tool exists to prevent, now on the replay path. The streams are still
    // retrievable regardless, so the view is still shown; treating an unknown
    // fate as failed (not succeeded) only affects filtering — a failing
    // command's stderr is force-kept — and the process's own exit code.
    let exit_code = meta.as_ref().map(|m| m.exit_code);

    if level == ViewLevel::Raw {
        // Byte-identical to what the command produced. No parse, no re-encode:
        // this path is what makes the store's promise checkable.
        emit(&stdout, &stderr);
    } else {
        let view = filter(&stdout, &stderr, exit_code.unwrap_or(1), level, Some(handle.as_str()));
        emit(&view.stdout, &view.stderr);
    }

    match exit_code {
        // Reading a run is not running it, but reporting success for a run
        // that failed would be a lie, so the stored code is what this exits
        // with.
        Some(code) => exit_with(code),
        None => {
            eprintln!("lens: run `{handle}` has no recorded exit code — treating as failed");
            exit_with(1)
        }
    }
}

/// A rendered view of both streams, and what it cost.
struct View {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Lines the command produced.
    in_lines: usize,
    /// Lines that reached the caller.
    out_lines: usize,
    /// Estimated tokens in, and out.
    in_tok: usize,
    out_tok: usize,
    /// Stages that ran, in order.
    stages: Vec<String>,
}

/// Parse, filter and render both streams at `level`.
///
/// The two are filtered separately and never merged: which stream a line came
/// from is a signal the pipeline uses, and the caller gets them back on the
/// descriptors the command wrote them to.
fn filter(
    stdout: &[u8],
    stderr: &[u8],
    exit_code: i32,
    level: ViewLevel,
    handle: Option<&str>,
) -> View {
    let ctx = Ctx { exit_code, ..Ctx::default() };
    let stages = lens::pipeline::default_stages();

    let out = filter_stream(stdout, Stream::Stdout, &stages, &ctx, level, handle);
    let err = filter_stream(stderr, Stream::Stderr, &stages, &ctx, level, handle);

    let estimator = Heuristic;
    View {
        // Measured on the rendered view rather than on the kept blocks: the
        // marker lines are part of what the caller pays for, and a reduction
        // figure that omits them would flatter the tool.
        in_tok: estimator.estimate(&String::from_utf8_lossy(stdout))
            + estimator.estimate(&String::from_utf8_lossy(stderr)),
        out_tok: estimator.estimate(&String::from_utf8_lossy(&out.bytes))
            + estimator.estimate(&String::from_utf8_lossy(&err.bytes)),
        in_lines: out.in_lines + err.in_lines,
        out_lines: out.out_lines + err.out_lines,
        stages: stages.iter().map(|stage| stage.name().to_string()).collect(),
        stdout: out.bytes,
        stderr: err.bytes,
    }
}

/// One filtered stream.
struct StreamView {
    bytes: Vec<u8>,
    in_lines: usize,
    out_lines: usize,
}

/// Filter one stream, or pass it through when it is not text.
///
/// Output that is not valid UTF-8 is emitted unchanged. Filtering it would mean
/// deciding what a byte sequence means, and the honest answer is that Lens does
/// not know — a tarball, a binary diff or a compressed stream is content whose
/// every byte matters. Mangling it into replacement characters to save tokens
/// would break the command for the sake of reading it, so this is one more case
/// where doubt resolves to passthrough.
fn filter_stream(
    raw: &[u8],
    stream: Stream,
    stages: &[&dyn lens::pipeline::Stage],
    ctx: &Ctx,
    level: ViewLevel,
    handle: Option<&str>,
) -> StreamView {
    let lines = raw.iter().filter(|b| **b == b'\n').count();

    if std::str::from_utf8(raw).is_err() {
        return StreamView { bytes: raw.to_vec(), in_lines: lines, out_lines: lines };
    }

    let mut doc = lens::adapters::parse(raw, stream);
    lens::pipeline::run(&mut doc, stages, ctx);
    let rendered = lens::render::render(&doc, level, handle);

    StreamView {
        bytes: rendered.into_bytes(),
        in_lines: doc.line_count(),
        out_lines: doc.kept_line_count(),
    }
}

/// `LENS_LEVEL`, when set to a level this build understands.
fn level_from_env() -> Option<u8> {
    std::env::var_os("LENS_LEVEL")?.to_string_lossy().parse().ok()
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
                Some(spec) => match lens::log::since_cutoff(spec, std::time::SystemTime::now()) {
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

    let records = lens::log::read_all(&env.logs, lens::log::DEFAULT_MAX_FILES);
    let stats = lens::log::aggregate(&records, since.as_deref(), cmd.as_deref());

    if stats.runs == 0 {
        println!("no runs recorded in {}", env.logs.display());
        return ExitCode::SUCCESS;
    }

    let pct = |part: u64| (part as f64 / stats.runs as f64) * 100.0;
    println!("runs              {:>10}", stats.runs);
    println!("passthrough       {:>10}  ({:.0}%)", stats.passthrough, pct(stats.passthrough));
    println!("captured bytes    {:>10}", stats.bytes);
    if stats.in_tok > 0 {
        let reduction = 100.0 - (stats.out_tok as f64 / stats.in_tok as f64) * 100.0;
        println!("input tokens      {:>10}", stats.in_tok);
        println!("output tokens     {:>10}", stats.out_tok);
        // Output tokens only, and labelled as such. Prompt caching and extra
        // turns both break the inference from this number to a bill.
        println!("reduction         {reduction:>9.1}%  (output tokens only)");
    }

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

    let records = lens::log::read_all(&env.logs, lens::log::DEFAULT_MAX_FILES);
    let matching: Vec<&lens::log::Record> =
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
    exec_or_report(cmd, &argv[0])
}

/// Run a command line that never went through `cli::parse`, because a
/// non-UTF-8 byte in it made that impossible.
///
/// Everything Lens's own store and log would record about a run is
/// `String`-typed, so there is no faithful way to write a record for this one
/// either — the command still runs, it is simply not captured or filtered.
/// That trade is invariant 6 in the one case it forces: doubt about how to
/// represent a command line is not a reason to refuse to run it.
fn exec_raw(argv: &[std::ffi::OsString]) -> ExitCode {
    let Some(program) = argv.first() else {
        // Unreachable: an empty `argv` decodes to an empty (valid) Vec<String>
        // above and never reaches this function.
        return fail(&"lens: no command");
    };
    let mut cmd = Command::new(program);
    cmd.args(&argv[1..]);
    exec_or_report(cmd, program.to_string_lossy().as_ref())
}

/// Hand `cmd` to the platform's passthrough and turn the outcome into an
/// [`ExitCode`], reporting `name` if it could not be started.
fn exec_or_report(mut cmd: Command, name: &str) -> ExitCode {
    match lens::platform::passthrough(&mut cmd) {
        lens::platform::Passthrough::Completed(status) => {
            exit_with(lens::platform::exit_code_for_status(&status))
        }
        lens::platform::Passthrough::FailedToStart(err) => {
            // exec only returns on failure. A command that could not be
            // started is the shell's classic 127.
            eprintln!("lens: {name}: {err}");
            ExitCode::from(127)
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
  lens show <handle> [--level N]  re-derive a view without re-running it
  lens stats [--since 7d] [--cmd git]
  lens logs [--tail N] [--level warn]

lens flags go before the command name. Everything after it belongs to the
child, including tokens that look like lens flags.

environment:
  LENS_MODE=raw                 emit raw output, exit with the child's code
  LENS_LEVEL=0..3               how much detail to show (default 2; 3 is raw)
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
