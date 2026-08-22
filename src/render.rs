// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Turning a filtered document back into output.
//!
//! This is the only place anything is actually removed. Stages mark; the
//! renderer subtracts, and because it subtracts it knows exactly how much went
//! and can say so.
//!
//! The marker is the load-bearing part of the whole design. It is what tells a
//! reader that the view is a view — that there is more, that it is still there,
//! and how to ask for it. Phrasing matters: content is *outside the view*, never
//! destroyed. A tool that quietly truncates is a tool nobody can trust with the
//! output they did not read.

use crate::pipeline::{Class, Doc};

/// How much detail to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Counts and outcome only.
    Summary,
    /// One line per kept item: failures and the shape of the rest.
    Items,
    /// Everything the pipeline kept.
    Detail,
    /// The raw stream, byte for byte.
    Raw,
}

impl Level {
    /// Parse a numeric level, as `--level N` gives it.
    pub fn from_number(n: u8) -> Option<Self> {
        match n {
            0 => Some(Level::Summary),
            1 => Some(Level::Items),
            2 => Some(Level::Detail),
            3 => Some(Level::Raw),
            _ => None,
        }
    }

    /// The number this level is written as.
    pub fn number(self) -> u8 {
        match self {
            Level::Summary => 0,
            Level::Items => 1,
            Level::Detail => 2,
            Level::Raw => 3,
        }
    }
}

/// The default level.
///
/// Detail, until a budget exists to choose for itself. Picking the fullest
/// filtered view means the first thing a reader loses is noise, not content.
pub const DEFAULT_LEVEL: Level = Level::Detail;

/// Render `doc` at `level`, announcing anything left out.
///
/// `handle` goes into every marker, because a marker that says content exists
/// without saying how to reach it is only half an announcement.
pub fn render(doc: &Doc, level: Level, handle: Option<&str>) -> String {
    // A stream the command never wrote to has nothing to report, at any level.
    // Announcing "0 lines" on stderr for a command that succeeded quietly is
    // noise the caller then has to filter, which is the wrong way round.
    if doc.blocks.is_empty() {
        return String::new();
    }

    match level {
        Level::Summary => summary(doc, handle),
        Level::Items => body(doc, handle, true),
        Level::Detail | Level::Raw => body(doc, handle, false),
    }
}

/// Counts only: what happened, not what was said.
fn summary(doc: &Doc, handle: Option<&str>) -> String {
    let failures = doc.blocks.iter().filter(|b| is_failure(b.class)).count();
    let warnings = doc.blocks.iter().filter(|b| b.class == Class::Warning).count();
    let mut out = format!(
        "{} lines · {failures} failing · {warnings} warning{}",
        doc.line_count(),
        if warnings == 1 { "" } else { "s" }
    );

    if doc.line_count() > 0 {
        out.push('\n');
        let mut all =
            Pending { lines: doc.line_count(), blocks: doc.blocks.len(), ..Default::default() };
        all.reason = Some("this view is counts only");
        out.push_str(&marker(&all, handle));
        out.push('\n');
    }
    out
}

/// The kept blocks, with a marker wherever a run of them was left out.
///
/// Markers are placed inline rather than gathered at the end, so the reader
/// learns *where* the gap is. A summary at the bottom says how much is missing;
/// a marker in position says what it was between.
fn body(doc: &Doc, handle: Option<&str>, failures_only: bool) -> String {
    let mut out = String::new();
    let mut pending = Pending::default();
    let mut wrote_anything = false;

    for block in &doc.blocks {
        let show = block.kept() && !(failures_only && !is_failure(block.class));

        if !show {
            pending.add(block);
            continue;
        }

        if pending.lines > 0 {
            out.push_str(&marker(&pending, handle));
            out.push('\n');
            pending = Pending::default();
        }

        for line in &block.lines {
            out.push_str(&line.text);
            out.push('\n');
        }
        wrote_anything = true;
    }

    if pending.lines > 0 {
        // Trailing elisions are announced too. A view that ends early without
        // saying so is exactly the failure this tool exists to avoid.
        out.push_str(&marker(&pending, handle));
        out.push('\n');
    }

    if !wrote_anything && out.is_empty() && doc.line_count() > 0 {
        let mut all = Pending::default();
        for block in &doc.blocks {
            all.add(block);
        }
        all.reason = Some("nothing in this view");
        out.push_str(&marker(&all, handle));
        out.push('\n');
    }

    out
}

/// A run of blocks the view is leaving out, and why.
#[derive(Default)]
struct Pending {
    lines: usize,
    blocks: usize,
    /// The single reason every block in the run shares, if they share one.
    reason: Option<&'static str>,
    mixed: bool,
}

impl Pending {
    fn add(&mut self, block: &crate::pipeline::Block) {
        self.lines += block.lines.len();
        self.blocks += 1;

        let reason = block.elided.as_ref().map(|e| e.reason);
        match (self.reason, reason) {
            (None, Some(r)) if !self.mixed => self.reason = Some(r),
            (Some(existing), Some(r)) if existing != r => {
                self.reason = None;
                self.mixed = true;
            }
            _ => {}
        }
    }
}

/// A single elision marker.
///
/// One line, bracketed, prefixed `[lens:`. Anything that pipes filtered output
/// into a parser fails loudly on this line rather than silently processing a
/// truncated stream, which is the second reason it exists.
///
/// It names the handle and **does not name a command**. The first version ended
/// with `lens show <handle> --level 3`, and the retention benchmark showed
/// agents reading that as the next step and taking it — fetching the entire raw
/// output, paying for both views and an extra turn, and undoing the filtering
/// completely. A marker's job is to say what is missing well enough that the
/// reader can tell whether it needs it; an instruction to fetch everything
/// answers that question for them, and answers it wrong.
///
/// So the text describes the content rather than offering a command. "1,199
/// further repetitions of the block above" is a reason not to ask.
fn marker(pending: &Pending, handle: Option<&str>) -> String {
    let what = describe(pending);
    match handle {
        Some(handle) => format!("[lens: {what} · handle {handle}]"),
        None => format!("[lens: {what}]"),
    }
}

/// What was left out, in terms of what it was.
fn describe(pending: &Pending) -> String {
    let lines = pending.lines;
    let plural = if lines == 1 { "" } else { "s" };

    // "not shown" rather than "removed": the content is outside this view, not
    // gone, and the wording is the only place most readers meet that claim.
    match pending.reason {
        Some("dedupe") => format!("{lines} further repeated line{plural} not shown"),
        Some("progress") => format!("{lines} progress line{plural} not shown"),
        Some(other) => format!("{lines} line{plural} not shown · {other}"),
        None if pending.blocks > 1 => {
            format!("{lines} line{plural} in {} blocks not shown", pending.blocks)
        }
        None => format!("{lines} line{plural} not shown"),
    }
}

fn is_failure(class: Class) -> bool {
    matches!(class, Class::Error | Class::Failure)
}

/// Does this output announce an elision?
#[cfg(test)]
pub fn has_marker(text: &str) -> bool {
    text.lines().any(|line| line.trim_start().starts_with("[lens:"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{Ctx, Stage, Stream, classify, default_stages, progress, run};

    fn doc(text: &str) -> Doc {
        crate::adapters::parse(text.as_bytes(), Stream::Stdout)
    }

    fn filtered(text: &str, exit_code: i32) -> Doc {
        let mut d = doc(text);
        let ctx = Ctx { exit_code, ..Ctx::default() };
        run(&mut d, &default_stages(), &ctx);
        d
    }

    #[test]
    fn levels_parse_from_numbers() {
        assert_eq!(Level::from_number(0), Some(Level::Summary));
        assert_eq!(Level::from_number(3), Some(Level::Raw));
        assert_eq!(Level::from_number(4), None);
        for level in [Level::Summary, Level::Items, Level::Detail, Level::Raw] {
            assert_eq!(Level::from_number(level.number()), Some(level));
        }
    }

    #[test]
    fn an_unfiltered_document_renders_unchanged_and_says_nothing() {
        // No elision, so no marker. A marker on output that lost nothing would
        // train readers to ignore markers.
        let out = render(&doc("one\ntwo\nthree"), Level::Detail, Some("a3f19c2b"));
        assert_eq!(out, "one\ntwo\nthree\n");
        assert!(!has_marker(&out));
    }

    #[test]
    fn anything_removed_is_announced() {
        let d = filtered("   Compiling foo v1.0\n\nreal output\n", 0);
        let out = render(&d, Level::Detail, Some("a3f19c2b"));
        assert!(has_marker(&out), "{out}");
        assert!(out.contains("real output"));
    }

    #[test]
    fn a_marker_says_what_kind_of_content_is_missing() {
        // Enough for the reader to decide it does not need the rest. "1,199
        // further repeated lines" is a reason not to ask; "1,199 lines" is not.
        let d = filtered("kept\n\nsame\nsame\nsame\nsame\n", 0);
        let out = render(&d, Level::Detail, Some("h"));
        let line = out.lines().find(|l| l.starts_with("[lens:")).expect("a marker");
        assert!(line.contains("repeated"), "{line}");
    }

    #[test]
    fn a_marker_carries_the_handle_and_the_count() {
        // Both halves of the announcement: that content exists, and how to get
        // it. Either alone is useless.
        let d = filtered("   Compiling a v1.0\n\n   Compiling b v1.0\n\nkept\n", 0);
        let out = render(&d, Level::Detail, Some("a3f19c2b"));
        let line = out.lines().find(|l| l.starts_with("[lens:")).expect("a marker");
        assert!(line.contains("a3f19c2b"), "the handle: {line}");
        assert!(line.contains('2'), "the line count: {line}");
        // No command. An agent reading `lens show <handle> --level 3` here took
        // it as the next step and fetched the entire raw output, which is the
        // one outcome filtering exists to avoid.
        assert!(!line.contains("lens show"), "a marker offers, it does not instruct: {line}");
        assert!(!line.contains("--level"), "{line}");
    }

    #[test]
    fn a_marker_is_phrased_as_content_outside_the_view() {
        // The tool's central claim, in the one place a reader meets it.
        let d = filtered("   Compiling foo v1.0\n\nkept\n", 0);
        let out = render(&d, Level::Detail, Some("h"));
        let line = out.lines().find(|l| l.starts_with("[lens:")).unwrap();
        assert!(line.contains("not shown"), "{line}");
        for word in ["deleted", "removed", "truncated", "discarded", "lost"] {
            assert!(!line.contains(word), "{line} suggests loss");
        }
    }

    #[test]
    fn markers_sit_where_the_gap_is() {
        // Position is information: the reader learns what the missing content
        // was between, not merely that some exists.
        let d = filtered("first\n\n   Compiling foo v1.0\n\nlast\n", 0);
        let out = render(&d, Level::Detail, Some("h"));
        let lines: Vec<&str> = out.lines().collect();
        let marker_at = lines.iter().position(|l| l.starts_with("[lens:")).unwrap();
        let first_at = lines.iter().position(|l| *l == "first").unwrap();
        let last_at = lines.iter().position(|l| *l == "last").unwrap();
        assert!(first_at < marker_at && marker_at < last_at, "{out}");
    }

    #[test]
    fn a_trailing_elision_is_announced() {
        // The view ends early; saying nothing here is the exact failure the
        // marker exists to prevent.
        let d = filtered("kept\n\n   Compiling foo v1.0\n", 0);
        let out = render(&d, Level::Detail, Some("h"));
        assert!(out.trim_end().ends_with(']'), "{out}");
    }

    #[test]
    fn the_summary_level_reports_counts() {
        let d = filtered("error: boom\n\nwarning: hm\n\nfine\n", 1);
        let out = render(&d, Level::Summary, Some("h"));
        assert!(out.contains("1 failing"), "{out}");
        assert!(out.contains("1 warning"), "{out}");
        assert!(has_marker(&out), "a summary is the largest elision there is");
    }

    #[test]
    fn the_items_level_keeps_failures_and_announces_the_rest() {
        let d = filtered("setup\n\nerror: boom\n\nteardown\n", 1);
        let out = render(&d, Level::Items, Some("h"));
        assert!(out.contains("error: boom"));
        assert!(!out.contains("teardown"));
        assert!(has_marker(&out));
    }

    #[test]
    fn a_view_with_nothing_in_it_still_announces() {
        // Empty output where the command produced plenty would otherwise read
        // as "the command said nothing".
        let d = filtered("just some ordinary output\n", 0);
        let out = render(&d, Level::Items, Some("h"));
        assert!(has_marker(&out), "{out:?}");
    }

    #[test]
    fn an_empty_stream_renders_empty_at_every_level() {
        // A command that wrote nothing to stderr must not have stderr output
        // invented for it — least of all a summary saying so.
        for level in [Level::Summary, Level::Items, Level::Detail, Level::Raw] {
            let out = render(&doc(""), level, Some("h"));
            assert!(out.is_empty(), "level {} rendered {out:?}", level.number());
        }
    }

    #[test]
    fn a_marker_without_a_handle_still_announces() {
        let d = filtered("   Compiling foo v1.0\n\nkept\n", 0);
        let out = render(&d, Level::Detail, None);
        assert!(has_marker(&out));
        assert!(!out.contains("handle"), "no handle, nothing to name");
    }

    #[test]
    fn every_kept_line_appears_exactly_once() {
        let d = filtered("alpha\n\nbeta\n\ngamma\n", 0);
        let out = render(&d, Level::Detail, Some("h"));
        for word in ["alpha", "beta", "gamma"] {
            assert_eq!(out.matches(word).count(), 1, "{word} in {out}");
        }
    }

    #[test]
    fn a_failing_command_shows_its_failure_at_every_filtered_level() {
        // The worst possible bug, checked at each level that claims to be a
        // useful view of a run.
        let text = "step one\n\nstep two\n\nerror: the thing broke\n";
        let d = filtered(text, 1);
        for level in [Level::Items, Level::Detail] {
            let out = render(&d, level, Some("h"));
            assert!(out.contains("the thing broke"), "level {}: {out}", level.number());
        }
    }

    #[test]
    fn a_failure_with_no_recognizable_error_still_surfaces() {
        // Exit code 1, nothing that looks like an error: the floor forces the
        // tail, and the renderer has to show it.
        let text = "doing a thing\n\nand another\n\nthe last thing it said\n";
        let d = filtered(text, 1);
        let out = render(&d, Level::Detail, Some("h"));
        assert!(out.contains("the last thing it said"), "{out}");
    }

    #[test]
    fn progress_only_output_collapses_to_a_marker() {
        let mut d = doc("   Compiling a v1.0\n\n   Compiling b v1.0\n\n   Compiling c v1.0\n");
        let ctx = Ctx::default();
        progress::Progress.apply(&mut d, &ctx);
        classify::Classify.apply(&mut d, &ctx);

        let out = render(&d, Level::Detail, Some("h"));
        assert!(has_marker(&out));
        assert!(!out.contains("Compiling"));
    }
}
