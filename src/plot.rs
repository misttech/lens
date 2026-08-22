// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Visualize the pipeline that will run — or that did run.
//!
//! Dry mode resolves config and prints it. It never spawns a child, which is
//! what makes `lens plot git push` safe. Trace mode reads a stored run and
//! annotates the same picture with per-stage counts. Both print the
//! [`crate::config::ResolvedPipeline`] the runner executes, so the picture
//! cannot drift from the run.

use crate::config::ResolvedPipeline;
use crate::pipeline::{self, Ctx, Doc, Stream};
use crate::tokens::{Heuristic, TokenEstimator};

/// How to print the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable text.
    Text,
    /// The resolved pipeline as data.
    Json,
}

impl Format {
    /// Parse `--format`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Format::Text),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

/// Dry picture: what would run, not what did.
pub fn dry(resolved: &ResolvedPipeline, format: Format, boxes: bool) -> String {
    match format {
        Format::Json => {
            let mut json = serde_json::to_string_pretty(resolved).unwrap_or_else(|_| "{}".into());
            if !json.ends_with('\n') {
                json.push('\n');
            }
            json
        }
        Format::Text => dry_text(resolved, boxes),
    }
}

/// Trace picture: the same graph, with counts from a stored run.
pub fn trace(
    resolved: &ResolvedPipeline,
    stdout: &[u8],
    stderr: &[u8],
    exit_code: i32,
    format: Format,
    boxes: bool,
) -> String {
    match format {
        Format::Json => {
            let steps = collect_trace(resolved, stdout, stderr, exit_code);
            let mut json = serde_json::json!({
                "pipeline": resolved,
                "exit": exit_code,
                "steps": steps,
            })
            .to_string();
            json.push('\n');
            json
        }
        Format::Text => {
            let mut out = dry_text(resolved, boxes);
            out.push('\n');
            out.push_str(&trace_text(resolved, stdout, stderr, exit_code, boxes));
            out
        }
    }
}

fn dry_text(resolved: &ResolvedPipeline, boxes: bool) -> String {
    let cmd = resolved.argv.join(" ");
    let mut out = format!("lens plot  cmd=\"{cmd}\"\n\n");
    out.push_str("resolution\n");
    out.push_str(&row("lens", &resolved.lens.value, &resolved.lens.source.label()));
    out.push_str(&row("adapter", &resolved.adapter.value, &resolved.adapter.source.label()));
    let budget = match resolved.budget.value {
        Some(n) => format!("{n} tokens"),
        None => "none".into(),
    };
    out.push_str(&row("budget", &budget, &resolved.budget.source.label()));
    out.push_str(&row(
        "context_lines",
        &resolved.context_blocks.value.to_string(),
        &resolved.context_blocks.source.label(),
    ));

    out.push_str("\npipeline\n");
    let names = &resolved.stages.value;
    for (i, name) in names.iter().enumerate() {
        let branch = if boxes {
            if i + 1 == names.len() { "└─" } else { "├─" }
        } else if i + 1 == names.len() {
            "`-"
        } else {
            "|-"
        };
        let src = if pipeline::stage_named(name).is_some() || name == "budget" {
            resolved.stages.source.label()
        } else {
            "skipped (not in this build)".into()
        };
        out.push_str(&format!("  {branch} {name:<12} {src}\n"));
    }

    let default: Vec<String> =
        pipeline::default_stage_names().iter().map(|s| (*s).to_string()).collect();
    let disabled: Vec<&String> = default.iter().filter(|n| !names.contains(n)).collect();
    if !disabled.is_empty() {
        let list = disabled.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("\n  disabled: {list}\n"));
        out.push_str(&format!(
            "            <- lens \"{}\" overrides the default stage list\n",
            resolved.lens.value
        ));
    }
    out
}

fn row(key: &str, value: &str, source: &str) -> String {
    format!("  {key:<14} {value:<28} {source}\n")
}

#[derive(Debug, Clone, serde::Serialize)]
struct Step {
    name: String,
    lines: usize,
    tokens: usize,
}

fn collect_trace(
    resolved: &ResolvedPipeline,
    stdout: &[u8],
    stderr: &[u8],
    exit_code: i32,
) -> Vec<Step> {
    let ctx = Ctx {
        exit_code,
        context_blocks: resolved.context_blocks.value,
        budget: resolved.budget.value,
    };
    let mut out_doc = parse_stream(stdout, Stream::Stdout);
    let mut err_doc = parse_stream(stderr, Stream::Stderr);
    let estimator = Heuristic;
    let mut steps = Vec::new();

    let snapshot = |name: &str, out: &Doc, err: &Doc| -> Step {
        let lines = out.kept_line_count() + err.kept_line_count();
        let text = format!("{}\n{}", out_text(out), out_text(err));
        Step { name: name.into(), lines, tokens: estimator.estimate(&text) }
    };

    steps.push(snapshot("raw", &out_doc, &err_doc));

    for stage in resolved.runnable_stages() {
        stage.apply(&mut out_doc, &ctx);
        stage.apply(&mut err_doc, &ctx);
        steps.push(snapshot(stage.name(), &out_doc, &err_doc));
    }

    if resolved.wants_budget() && resolved.budget.value.is_some() {
        pipeline::budget::apply(&mut [&mut out_doc, &mut err_doc], &ctx);
        steps.push(snapshot("budget", &out_doc, &err_doc));
    }

    steps
}

fn trace_text(
    resolved: &ResolvedPipeline,
    stdout: &[u8],
    stderr: &[u8],
    exit_code: i32,
    boxes: bool,
) -> String {
    let steps = collect_trace(resolved, stdout, stderr, exit_code);
    let cmd = resolved.argv.join(" ");
    let mut out = format!("  {cmd}    exit {exit_code}\n");
    let pipe = if boxes { "│" } else { "|" };
    let tee = if boxes { "├─" } else { "|-" };
    let last = if boxes { "└─" } else { "`-" };

    for (i, step) in steps.iter().enumerate() {
        let branch = if i == 0 {
            format!("  {pipe}")
        } else if i + 1 == steps.len() {
            format!("  {last}")
        } else {
            format!("  {tee}")
        };
        if i == 0 {
            out.push_str(&format!(
                "  raw                        {:>6} tok  {:>5} ln\n",
                step.tokens, step.lines
            ));
            continue;
        }
        out.push_str(&format!(
            "{branch} {:<12}              {:>6} tok  {:>5} ln\n",
            step.name, step.tokens, step.lines
        ));
    }
    if let (Some(raw), Some(end)) = (steps.first(), steps.last())
        && raw.tokens > 0
    {
        let reduction = 100.0 - (end.tokens as f64 / raw.tokens as f64) * 100.0;
        out.push_str(&format!("\n  reduction  {reduction:.1}%\n"));
    }
    out
}

fn parse_stream(raw: &[u8], stream: Stream) -> Doc {
    if std::str::from_utf8(raw).is_err() {
        return Doc::empty(stream);
    }
    crate::adapters::parse(raw, stream)
}

fn out_text(doc: &Doc) -> String {
    doc.blocks.iter().filter(|b| b.kept()).map(|b| b.text()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, ResolveInput};
    use crate::platform::Dirs;
    use std::path::PathBuf;

    fn resolved(argv: &[&str]) -> ResolvedPipeline {
        let args: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        let dirs = Dirs {
            cache: PathBuf::from("/tmp"),
            config: PathBuf::from("/tmp"),
            state: PathBuf::from("/tmp"),
        };
        config::resolve(&ResolveInput {
            argv: &args,
            cwd: std::path::Path::new("/"),
            dirs: &dirs,
            config_override: None,
            cli_budget: None,
            cli_use: None,
            env_budget: None,
        })
        .unwrap()
    }

    #[test]
    fn dry_text_names_the_lens_and_the_stages() {
        let text = dry(&resolved(&["git", "diff"]), Format::Text, false);
        assert!(text.contains("cmd=\"git diff\""), "{text}");
        assert!(text.contains("git-diff"), "{text}");
        assert!(text.contains("classify"), "{text}");
        assert!(text.contains("disabled:"), "{text}");
        assert!(text.contains("progress"), "{text}");
    }

    #[test]
    fn dry_json_is_the_resolved_pipeline() {
        let json = dry(&resolved(&["git", "status"]), Format::Json, false);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["lens"]["value"], "git");
        assert_eq!(v["budget"]["value"], 4000);
    }

    #[test]
    fn trace_counts_shrink_when_progress_is_dropped() {
        let r = resolved(&["cargo", "test"]);
        let stdout = "   Compiling foo v1.0\n\n   Compiling bar v1.0\n\nerror: boom\n";
        let text = trace(&r, stdout.as_bytes(), b"", 1, Format::Text, false);
        assert!(text.contains("reduction"), "{text}");
        assert!(text.contains("progress") || text.contains("raw"), "{text}");
    }
}
