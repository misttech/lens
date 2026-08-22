// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The filtering report: what was kept, what was not, and which stage said so.
//!
//! Written to stderr after the child's output has flushed (`LENS_DEBUG=1`), or
//! to stdout by `lens explain` for a past run. Either way it is a report about
//! a view, not a view itself — it never shares a stream with the command.

use std::collections::BTreeMap;

use crate::pipeline::{Class, Doc, Keep};

/// What a run did to its output, in the terms a reader can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Stored run, when there is one.
    pub handle: Option<String>,
    /// Command name, for the header.
    pub cmd: String,
    /// The child's exit code.
    pub exit: i32,
    /// How long the child took, when known.
    pub dur_ms: Option<u64>,
    /// Estimated tokens in, and out.
    pub in_tok: usize,
    /// Estimated tokens that reached the caller.
    pub out_tok: usize,
    /// Lines the command produced.
    pub in_lines: usize,
    /// Lines that reached the caller.
    pub out_lines: usize,
    /// Lines dropped, keyed by the reason the stage recorded.
    pub dropped: BTreeMap<String, usize>,
    /// Blocks classified as a failure.
    pub errors: usize,
    /// Blocks classified as a warning.
    pub warnings: usize,
    /// Blocks force-kept by context (or the failure floor).
    pub forced: usize,
    /// The irreducible core still exceeded the budget.
    pub budget_exceeded: bool,
}

impl Report {
    /// Build a report from the documents the pipeline just filtered.
    pub fn from_docs(
        docs: &[&Doc],
        handle: Option<&str>,
        cmd: &str,
        exit: i32,
        dur_ms: Option<u64>,
        in_tok: usize,
        out_tok: usize,
    ) -> Self {
        let mut dropped: BTreeMap<String, usize> = BTreeMap::new();
        let mut errors = 0;
        let mut warnings = 0;
        let mut forced = 0;
        let mut in_lines = 0;
        let mut out_lines = 0;
        let mut budget_exceeded = false;

        for doc in docs {
            in_lines += doc.line_count();
            out_lines += doc.kept_line_count();
            budget_exceeded |= doc.budget_exceeded;
            for block in &doc.blocks {
                if matches!(block.class, Class::Error | Class::Failure) {
                    errors += 1;
                }
                if block.class == Class::Warning {
                    warnings += 1;
                }
                if block.keep == Keep::Force {
                    forced += 1;
                }
                if let Some(elision) = &block.elided {
                    *dropped.entry(elision.reason.to_string()).or_insert(0) +=
                        elision.lines_removed;
                }
            }
        }

        Report {
            handle: handle.map(str::to_string),
            cmd: cmd.to_string(),
            exit,
            dur_ms,
            in_tok,
            out_tok,
            in_lines,
            out_lines,
            dropped,
            errors,
            warnings,
            forced,
            budget_exceeded,
        }
    }

    /// The report as it is printed.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let handle = self.handle.as_deref().unwrap_or("-");
        let dur = self.dur_ms.map(|ms| format!("{ms}ms")).unwrap_or_else(|| "-".into());
        out.push_str(&format!(
            "lens report  handle={handle}  cmd=\"{}\"  exit={}  {dur}\n\n",
            self.cmd, self.exit
        ));

        let reduction = if self.in_tok > 0 {
            100.0 - (self.out_tok as f64 / self.in_tok as f64) * 100.0
        } else {
            0.0
        };

        out.push_str(&format!(
            "input        {:>10} tokens  {:>8} lines\n",
            fmt_num(self.in_tok),
            fmt_num(self.in_lines)
        ));
        out.push_str(&format!(
            "output       {:>10} tokens  {:>8} lines\n",
            fmt_num(self.out_tok),
            fmt_num(self.out_lines)
        ));
        out.push_str(&format!("reduction    {reduction:>9.1}%\n"));

        if !self.dropped.is_empty() {
            out.push_str("\nremoved by stage\n");
            for (reason, lines) in &self.dropped {
                out.push_str(&format!("  {reason:<22} {lines:>6} lines\n"));
            }
        }

        out.push_str("\npreserved\n");
        out.push_str(&format!("  errors                {:>6}\n", self.errors));
        out.push_str(&format!("  warnings              {:>6}\n", self.warnings));
        out.push_str(&format!("  forced by context     {:>6}\n", self.forced));
        if self.budget_exceeded {
            out.push_str("  budget exceeded          yes\n");
        }
        out
    }
}

fn fmt_num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{Block, Line, Stream};

    fn block(text: &str, class: Class, keep: Keep, reason: Option<&'static str>) -> Block {
        let mut b = Block::new(vec![Line { text: text.into(), origin: 1 }]);
        b.class = class;
        if let Some(reason) = reason {
            b.drop_with(reason);
        } else if keep == Keep::Force {
            b.force();
        }
        b
    }

    #[test]
    fn the_report_names_the_stage_that_dropped_lines() {
        let mut doc = crate::pipeline::Doc::empty(Stream::Stdout);
        doc.blocks.push(block("error: boom", Class::Error, Keep::Force, None));
        doc.blocks.push(block("   Compiling x", Class::Progress, Keep::Drop, Some("progress")));
        doc.blocks.push(block("same", Class::Info, Keep::Drop, Some("dedupe")));
        doc.blocks.push(block("same", Class::Info, Keep::Drop, Some("dedupe")));

        let report = Report::from_docs(&[&doc], Some("a3f19c2b"), "cargo", 1, Some(8), 100, 20);
        let text = report.render();
        assert!(text.contains("handle=a3f19c2b"), "{text}");
        assert!(text.contains("cmd=\"cargo\""), "{text}");
        assert!(text.contains("progress"), "{text}");
        assert!(text.contains("dedupe"), "{text}");
        assert!(text.contains("errors"), "{text}");
        assert!(text.contains("1"), "{text}");
    }

    #[test]
    fn budget_exceeded_is_stated() {
        let mut doc = crate::pipeline::Doc::empty(Stream::Stdout);
        doc.budget_exceeded = true;
        doc.blocks.push(block("error: boom", Class::Error, Keep::Force, None));
        let text = Report::from_docs(&[&doc], None, "x", 1, None, 50, 50).render();
        assert!(text.contains("budget exceeded"), "{text}");
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(fmt_num(18420), "18,420");
        assert_eq!(fmt_num(118), "118");
        assert_eq!(fmt_num(0), "0");
    }
}
