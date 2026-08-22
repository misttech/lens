// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The `lens` binary: argument handling, process control, and the decisions
//! that need an environment. Everything it filters with lives in the library.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use lens::cli::{Invocation, Subcommand};
use lens::config::{self, ResolveInput, ResolvedPipeline};
use lens::log::{Level, Logger, RunRecord};
use lens::pipeline::{Ctx, Stream};
use lens::plot::{self, Format as PlotFormat};
use lens::render::Level as ViewLevel;
use lens::resolve::{PassthroughReason, Plan};
use lens::store::Store;
use lens::tokens::{Heuristic, TokenEstimator};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

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
        Invocation::Subcommand { name: Subcommand::Show, args, budget, use_lens } => {
            show(&args, budget, use_lens)
        }
        Invocation::Subcommand { name: Subcommand::Explain, args, budget, use_lens } => {
            explain(&args, budget, use_lens)
        }
        Invocation::Subcommand { name: Subcommand::Stats, args, .. } => stats(&args),
        Invocation::Subcommand { name: Subcommand::Logs, args, .. } => logs(&args),
        Invocation::Subcommand { name: Subcommand::Plot, args, budget, use_lens } => {
            plot_cmd(&args, budget, use_lens)
        }
        Invocation::Subcommand { name: Subcommand::Lenses, args, .. } => lenses_cmd(&args),
        Invocation::Subcommand { name: Subcommand::Config, args, budget, use_lens } => {
            config_cmd(&args, budget, use_lens)
        }
        Invocation::Run { argv, budget, use_lens } => run(&argv, budget, use_lens),
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
fn run(argv: &[String], cli_budget: Option<usize>, use_lens: Option<String>) -> ExitCode {
    let resolved = match resolve_pipeline(argv, cli_budget, use_lens.as_deref()) {
        Ok(resolved) => resolved,
        Err(err) => return fail(&err),
    };
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
                let filtered = filter(
                    &captured.stdout,
                    &captured.stderr,
                    captured.exit_code,
                    level,
                    handle_str.as_deref(),
                    &resolved,
                );

                if let Some(err) = filtered.adapter_fallback.as_deref() {
                    logger.event(
                        Level::Warn,
                        "adapter parse failed, falling back to generic",
                        &[("cmd", command_name(argv)), ("err", err)],
                    );
                }

                logger.run(RunRecord {
                    handle: handle_str.clone(),
                    cmd: command_name(argv).to_string(),
                    argv: argv.to_vec(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    exit: Some(captured.exit_code),
                    dur_ms: Some(duration_ms),
                    out_bytes: Some(captured.stdout.len() as u64),
                    err_bytes: Some(captured.stderr.len() as u64),
                    in_lines: Some(filtered.view.in_lines as u64),
                    out_lines: Some(filtered.view.out_lines as u64),
                    in_tok: Some(filtered.view.in_tok as u64),
                    out_tok: Some(filtered.view.out_tok as u64),
                    level: Some(level.number()),
                    stages: filtered.view.stages.clone(),
                    passthrough: false,
                    reason: None,
                });

                emit(&filtered.view.stdout, &filtered.view.stderr);

                // After the child's output has flushed: a report mixed into
                // stderr mid-stream would be read as the command's.
                if debug_from_env() {
                    let report = lens::report::Report::from_docs(
                        &[&filtered.stdout, &filtered.stderr],
                        handle_str.as_deref(),
                        command_name(argv),
                        captured.exit_code,
                        Some(duration_ms),
                        filtered.view.in_tok,
                        filtered.view.out_tok,
                    );
                    eprint!("{}", report.render());
                }

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

/// The pipeline this invocation will run — the same object `plot` prints.
fn resolve_pipeline(
    argv: &[String],
    cli_budget: Option<usize>,
    use_lens: Option<&str>,
) -> Result<ResolvedPipeline, config::ResolveError> {
    let dirs = lens::platform::Dirs::from_env();
    let cwd = std::env::current_dir().unwrap_or_default();
    let config_override = std::env::var_os("LENS_CONFIG");
    config::resolve(&ResolveInput {
        argv,
        cwd: &cwd,
        dirs: &dirs,
        config_override: config_override.as_ref(),
        cli_budget,
        cli_use: use_lens,
        env_budget: budget_from_env(),
    })
}

/// `lens show <handle> [--level N]`.
///
/// Re-derives a view from stored bytes. The command is not re-executed, which is
/// the whole point: a reader who wants more detail pays for the render, not for
/// the run — and gets the same bytes however long ago it happened.
fn show(args: &[String], cli_budget: Option<usize>, use_lens: Option<String>) -> ExitCode {
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
    let exit_code = meta.as_ref().map(|m| m.exit_code).unwrap_or(0);
    let argv = meta.as_ref().map(|m| m.argv.clone()).unwrap_or_default();

    if level == ViewLevel::Raw {
        // Byte-identical to what the command produced. No parse, no re-encode:
        // this path is what makes the store's promise checkable.
        emit(&stdout, &stderr);
    } else {
        let resolved = match resolve_pipeline(&argv, cli_budget, use_lens.as_deref()) {
            Ok(resolved) => resolved,
            Err(err) => return fail(&err),
        };
        let view =
            filter(&stdout, &stderr, exit_code, level, Some(handle.as_str()), &resolved).view;
        emit(&view.stdout, &view.stderr);
    }

    // Reading a run is not running it, but reporting success for a run that
    // failed would be a lie, so the stored code is what this exits with.
    exit_with(exit_code)
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
/// descriptors the command wrote them to. The budget, when there is one, sees
/// both at once — it is a budget for the invocation, not for each pipe.
fn filter(
    stdout: &[u8],
    stderr: &[u8],
    exit_code: i32,
    level: ViewLevel,
    handle: Option<&str>,
    resolved: &ResolvedPipeline,
) -> Filtered {
    let ctx = Ctx {
        exit_code,
        context_blocks: resolved.context_blocks.value,
        budget: resolved.budget.value,
    };
    let stages = resolved.runnable_stages();
    let adapter = resolved.adapter.value.as_str();

    let (mut out_doc, out_fb) = pipeline_doc(stdout, Stream::Stdout, &stages, &ctx, adapter);
    let (mut err_doc, err_fb) = pipeline_doc(stderr, Stream::Stderr, &stages, &ctx, adapter);
    if resolved.budget.value.is_some() {
        lens::pipeline::budget::apply(&mut [&mut out_doc, &mut err_doc], &ctx);
    }

    let out = render_stream(stdout, &out_doc, level, handle);
    let err = render_stream(stderr, &err_doc, level, handle);

    let estimator = Heuristic;
    Filtered {
        view: View {
            in_tok: estimator.estimate(&String::from_utf8_lossy(stdout))
                + estimator.estimate(&String::from_utf8_lossy(stderr)),
            out_tok: estimator.estimate(&String::from_utf8_lossy(&out.bytes))
                + estimator.estimate(&String::from_utf8_lossy(&err.bytes)),
            in_lines: out.in_lines + err.in_lines,
            out_lines: out.out_lines + err.out_lines,
            stages: {
                let mut names: Vec<String> =
                    stages.iter().map(|stage| stage.name().to_string()).collect();
                if resolved.budget.value.is_some() {
                    names.push("budget".into());
                }
                names
            },
            stdout: out.bytes,
            stderr: err.bytes,
        },
        stdout: out_doc,
        stderr: err_doc,
        adapter_fallback: out_fb.or(err_fb),
    }
}

/// A rendered view of both streams, and the documents it was rendered from.
struct Filtered {
    view: View,
    stdout: lens::pipeline::Doc,
    stderr: lens::pipeline::Doc,
    /// Set when the named adapter could not parse and generic ran instead.
    adapter_fallback: Option<String>,
}

/// One filtered stream.
struct StreamView {
    bytes: Vec<u8>,
    in_lines: usize,
    out_lines: usize,
}

/// Filter one stream's document, or leave it empty when the bytes are not text.
///
/// Output that is not valid UTF-8 is emitted unchanged. Filtering it would mean
/// deciding what a byte sequence means, and the honest answer is that Lens does
/// not know — a tarball, a binary diff or a compressed stream is content whose
/// every byte matters. Mangling it into replacement characters to save tokens
/// would break the command for the sake of reading it, so this is one more case
/// where doubt resolves to passthrough.
fn pipeline_doc(
    raw: &[u8],
    stream: Stream,
    stages: &[&dyn lens::pipeline::Stage],
    ctx: &Ctx,
    adapter: &str,
) -> (lens::pipeline::Doc, Option<String>) {
    if std::str::from_utf8(raw).is_err() {
        return (lens::pipeline::Doc::empty(stream), None);
    }
    let (mut doc, fallback) = lens::adapters::parse_with(raw, stream, adapter);
    lens::pipeline::run(&mut doc, stages, ctx);
    (doc, fallback)
}

fn render_stream(
    raw: &[u8],
    doc: &lens::pipeline::Doc,
    level: ViewLevel,
    handle: Option<&str>,
) -> StreamView {
    let lines = raw.iter().filter(|b| **b == b'\n').count();
    if std::str::from_utf8(raw).is_err() {
        return StreamView { bytes: raw.to_vec(), in_lines: lines, out_lines: lines };
    }
    let rendered = lens::render::render(doc, level, handle);
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

/// `LENS_BUDGET`, when set to a token count.
fn budget_from_env() -> Option<usize> {
    std::env::var_os("LENS_BUDGET")?.to_string_lossy().parse().ok()
}

/// `LENS_DEBUG=1` (or `true`): emit the filtering report after the child.
fn debug_from_env() -> bool {
    std::env::var_os("LENS_DEBUG").is_some_and(|value| {
        let value = value.to_string_lossy();
        value == "1" || value.eq_ignore_ascii_case("true")
    })
}

/// `lens explain <handle>`.
///
/// Re-runs the pipeline against stored bytes and prints the report. The
/// command is not re-executed; the report describes the view of a run that
/// already happened.
fn explain(args: &[String], cli_budget: Option<usize>, use_lens: Option<String>) -> ExitCode {
    let env = Env::from_process();

    let mut handle_text: Option<&String> = None;
    for arg in args {
        match arg.as_str() {
            other if other.starts_with('-') => {
                return fail(&format!("unknown flag `{other}` for lens explain"));
            }
            _ => handle_text = Some(arg),
        }
    }

    let Some(text) = handle_text else {
        return fail(&"lens explain needs a handle, e.g. lens explain a3f19c2b");
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
    let exit_code = meta.as_ref().map(|m| m.exit_code).unwrap_or(1);
    let cmd = meta
        .as_ref()
        .and_then(|m| m.argv.first())
        .map(|name| name.rsplit('/').next().unwrap_or(name).to_string())
        .unwrap_or_else(|| "unknown".into());
    let dur_ms = meta.as_ref().map(|m| m.duration_ms);
    let argv = meta.as_ref().map(|m| m.argv.clone()).unwrap_or_default();
    let resolved = match resolve_pipeline(&argv, cli_budget, use_lens.as_deref()) {
        Ok(resolved) => resolved,
        Err(err) => return fail(&err),
    };

    let filtered = filter(
        &stdout,
        &stderr,
        exit_code,
        lens::render::DEFAULT_LEVEL,
        Some(handle.as_str()),
        &resolved,
    );
    let report = lens::report::Report::from_docs(
        &[&filtered.stdout, &filtered.stderr],
        Some(handle.as_str()),
        &cmd,
        exit_code,
        dur_ms,
        filtered.view.in_tok,
        filtered.view.out_tok,
    );
    print!("{}", report.render());
    ExitCode::SUCCESS
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

/// `lens plot [--format text|json] [--handle H] [command...]`.
///
/// Dry mode resolves config and prints it. It never spawns the command, which
/// is what makes `lens plot git push` safe. Trace mode (`--handle`) reads a
/// stored run and annotates the same picture with per-stage counts.
fn plot_cmd(args: &[String], cli_budget: Option<usize>, use_lens: Option<String>) -> ExitCode {
    let mut format = PlotFormat::Text;
    let mut handle_text: Option<String> = None;
    let mut rest = args.iter();
    let mut argv: Vec<String> = Vec::new();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--format" => match rest.next().and_then(|name| PlotFormat::parse(name)) {
                Some(parsed) => format = parsed,
                None => return fail(&"--format takes text or json"),
            },
            "--handle" => match rest.next() {
                Some(value) => handle_text = Some(value.clone()),
                None => return fail(&"--handle needs a handle, e.g. --handle a3f19c2b"),
            },
            other if other.starts_with('-') && argv.is_empty() => {
                return fail(&format!("unknown flag `{other}` for lens plot"));
            }
            _ => {
                argv.push(arg.clone());
                argv.extend(rest.cloned());
                break;
            }
        }
    }

    let boxes = want_boxes();

    if let Some(text) = handle_text {
        let env = Env::from_process();
        let Some(handle) = lens::store::Handle::parse(&text) else {
            return fail(&format!("`{text}` is not a handle — expected 8 hex digits"));
        };
        let store = Store::new(&env.store);
        let Ok(stdout) = store.read_stream(&handle, lens::store::Stream::Stdout) else {
            return fail(&format!("no run `{handle}` in {}", env.store.display()));
        };
        let stderr = store.read_stream(&handle, lens::store::Stream::Stderr).unwrap_or_default();
        let meta = store.read_meta(&handle).ok();
        let exit_code = meta.as_ref().map(|m| m.exit_code).unwrap_or(0);
        let stored_argv = meta.as_ref().map(|m| m.argv.clone()).filter(|a| !a.is_empty());
        let argv = if argv.is_empty() { stored_argv.unwrap_or_default() } else { argv };
        if argv.is_empty() {
            return fail(&"stored run has no argv to plot");
        }
        let resolved = match resolve_pipeline(&argv, cli_budget, use_lens.as_deref()) {
            Ok(resolved) => resolved,
            Err(err) => return fail(&err),
        };
        print!("{}", plot::trace(&resolved, &stdout, &stderr, exit_code, format, boxes));
        return ExitCode::SUCCESS;
    }

    if argv.is_empty() {
        return fail(&"lens plot needs a command, e.g. lens plot git diff");
    }
    let resolved = match resolve_pipeline(&argv, cli_budget, use_lens.as_deref()) {
        Ok(resolved) => resolved,
        Err(err) => return fail(&err),
    };
    print!("{}", plot::dry(&resolved, format, boxes));
    ExitCode::SUCCESS
}

/// Box-drawing only on a TTY whose TERM is not `dumb`. Tests and pipes get ASCII.
fn want_boxes() -> bool {
    #[cfg(unix)]
    {
        lens::platform::is_tty(lens::platform::STDOUT_FD)
            && std::env::var_os("TERM").is_some_and(|term| term != "dumb")
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// `lens lenses [--show NAME]`.
fn lenses_cmd(args: &[String]) -> ExitCode {
    let mut show: Option<String> = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--show" => match rest.next() {
                Some(name) => show = Some(name.clone()),
                None => return fail(&"--show needs a lens name"),
            },
            other if other.starts_with('-') => {
                return fail(&format!("unknown flag `{other}` for lens lenses"));
            }
            other => show = Some(other.to_string()),
        }
    }

    let dirs = lens::platform::Dirs::from_env();
    let cwd = std::env::current_dir().unwrap_or_default();
    let config_override = std::env::var_os("LENS_CONFIG");
    let dummy = vec!["true".into()];
    let input = ResolveInput {
        argv: &dummy,
        cwd: &cwd,
        dirs: &dirs,
        config_override: config_override.as_ref(),
        cli_budget: None,
        cli_use: None,
        env_budget: None,
    };
    let listed = config::list(&input);

    if let Some(name) = show {
        let Some((found, source, match_on)) = listed.iter().find(|(n, _, _)| n == &name) else {
            return fail(&format!("no lens named `{name}` — try: lens lenses"));
        };
        println!("{found}");
        println!("  source   {}", source.label());
        match match_on {
            Some(pat) => println!("  match    {pat}"),
            None => println!("  match    (none)"),
        }
        match resolve_pipeline(&dummy, None, Some(found)) {
            Ok(resolved) => {
                println!("  adapter  {}", resolved.adapter.value);
                match resolved.budget.value {
                    Some(n) => println!("  budget   {n}"),
                    None => println!("  budget   none"),
                }
                println!("  stages   {}", resolved.stages.value.join(", "));
            }
            Err(err) => return fail(&err),
        }
        return ExitCode::SUCCESS;
    }

    for (name, source, match_on) in listed {
        let pat = match_on.as_deref().unwrap_or("-");
        println!("{name:<16} {pat:<20} {}", source.label());
    }
    ExitCode::SUCCESS
}

/// `lens config [--path] [command...]`.
fn config_cmd(args: &[String], cli_budget: Option<usize>, use_lens: Option<String>) -> ExitCode {
    let mut path_only = false;
    let mut rest = args.iter();
    let mut argv: Vec<String> = Vec::new();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--path" => path_only = true,
            "--show" => {}
            other if other.starts_with('-') && argv.is_empty() => {
                return fail(&format!("unknown flag `{other}` for lens config"));
            }
            _ => {
                argv.push(arg.clone());
                argv.extend(rest.cloned());
                break;
            }
        }
    }

    if path_only {
        let dirs = lens::platform::Dirs::from_env();
        let config_override = std::env::var_os("LENS_CONFIG");
        println!("{}", dirs.config_file(config_override.as_ref()).display());
        return ExitCode::SUCCESS;
    }

    if argv.is_empty() {
        argv.push("true".into());
    }
    match resolve_pipeline(&argv, cli_budget, use_lens.as_deref()) {
        Ok(resolved) => match serde_json::to_string_pretty(&resolved) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(_) => fail(&"could not serialize resolved config"),
        },
        Err(err) => fail(&err),
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
        let err = lens::platform::exec(&mut cmd);
        // exec only returns on failure. A command that could not be started is
        // the shell's classic 127.
        eprintln!("lens: {}: {err}", argv[0]);
        ExitCode::from(127)
    }

    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(status) => exit_with(lens::platform::exit_code_for_status(&status)),
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
  lens [--budget N] [--use NAME] <command> [args...]
                                run a command and filter its output
  lens --version
  lens --help
  lens show <handle> [--level N]  re-derive a view without re-running it
  lens explain <handle>         filtering report for a past run
  lens stats [--since 7d] [--cmd git]
  lens plot [--format text|json] [--handle H] <command> [args...]
  lens lenses [--show NAME]
  lens config [--path] [command...]
  lens logs [--tail N] [--level warn]

lens flags go before the command name. Everything after it belongs to the
child, including tokens that look like lens flags.

environment:
  LENS_MODE=raw                 emit raw output, exit with the child's code
  LENS_LEVEL=0..3               how much detail to show (default 2; 3 is raw)
  LENS_BUDGET=<tokens>          drop lowest-ranked content to fit
  LENS_DEBUG=1                  filtering report on stderr after the child
  LENS_LOG=<level>              off error warn info debug trace (default info)
  LENS_STORE=<dir>              where runs are kept
  LENS_LOG_DIR=<dir>            where logs are kept
  LENS_CONFIG=<file>            override the user config file
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
        assert!(help.contains("lens plot"));
        assert!(!help.contains("not implemented"));
    }
}
