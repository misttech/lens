// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Drop progress indicators.
//!
//! Progress output is written for a human watching a terminal. By the time
//! anything reads it, it describes work that has already finished — the reader
//! learns nothing from `47%` that the final line does not say better.
//!
//! It is also the single largest source of volume in build and test output,
//! which makes it the cheapest thing this pipeline removes: high count, zero
//! decision-relevant content.

use super::{Class, Ctx, Doc, Stage};

/// The progress stage.
#[derive(Debug, Clone, Copy)]
pub struct Progress;

impl Stage for Progress {
    fn name(&self) -> &'static str {
        "progress"
    }

    fn apply(&self, doc: &mut Doc, _ctx: &Ctx) {
        for block in &mut doc.blocks {
            if block.lines.iter().all(|line| is_progress(&line.text)) {
                block.class = Class::Progress;
                block.drop_with("progress");
            }
        }
    }
}

/// Verbs that name work in flight rather than a result.
///
/// Matched at the start of a line, after leading whitespace, and only when
/// followed by an argument — `Compiling lens v0.1.0` is churn, but a bare
/// `Compiling` on its own is unusual enough to leave alone.
const PROGRESS_VERBS: &[&str] = &[
    "Compiling",
    "Downloading",
    "Downloaded",
    "Updating",
    "Fetching",
    "Installing",
    "Unpacking",
    "Resolving",
    "Building",
    "Waiting",
];

/// Is this line progress rather than content?
pub fn is_progress(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    if starts_with_progress_verb(trimmed) {
        return true;
    }
    if is_percentage(trimmed) {
        return true;
    }
    if is_bar(trimmed) {
        return true;
    }
    if is_spinner(trimmed) {
        return true;
    }
    false
}

/// `Compiling lens v0.1.0 (/path)`, `Downloading crates ...`
fn starts_with_progress_verb(text: &str) -> bool {
    PROGRESS_VERBS.iter().any(|verb| {
        text.strip_prefix(verb).is_some_and(|rest| rest.starts_with(' ') && rest.len() > 1)
    })
}

/// A line whose only claim is a percentage: ` 47%`, `47% done`, `[47%]`.
///
/// Requires the percentage to be most of the line. `error: 50% of tests failed`
/// contains a percentage and is not progress.
fn is_percentage(text: &str) -> bool {
    let Some(index) = text.find('%') else { return false };
    let before = &text[..index];

    let digits_end = before.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if digits_end.len() == before.len() {
        return false; // no digits immediately before the %
    }

    // Everything that is not the number itself has to be decoration.
    let leading = digits_end.trim();
    let trailing = text[index + 1..].trim();
    is_decoration(leading) && is_decoration(trailing)
}

/// A drawn bar: `[####----]`, `━━━━━━╾─────`, `=====>`.
fn is_bar(text: &str) -> bool {
    const BAR_CHARS: &[char] =
        &['#', '=', '-', '━', '─', '╾', '█', '░', '▒', '▓', '>', '[', ']', '|', '.'];
    let bar_like = text.chars().filter(|c| BAR_CHARS.contains(c)).count();
    // A long run of drawing characters, and little else.
    bar_like >= 8 && bar_like * 4 >= text.chars().count() * 3
}

/// A spinner frame, alone or leading a status word.
fn is_spinner(text: &str) -> bool {
    const SPINNERS: &[char] =
        &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '◐', '◓', '◑', '◒'];
    text.starts_with(SPINNERS)
}

/// Decoration around a number: brackets, arrows, and the words that pad them.
fn is_decoration(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    const WORDS: &[&str] = &["done", "complete", "completed", "of", "eta", "remaining"];
    text.split_whitespace().all(|word| {
        let stripped = word.trim_matches(|c: char| !c.is_alphanumeric());
        stripped.is_empty()
            || WORDS.contains(&stripped.to_ascii_lowercase().as_str())
            || stripped.chars().all(|c| c.is_ascii_digit() || c == '.')
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::*;

    #[test]
    fn build_churn_is_progress() {
        for line in [
            "   Compiling lens v0.1.0 (/home/u/lens)",
            "    Updating crates.io index",
            " Downloading 42 crates",
            "  Installing lens v0.1.0",
        ] {
            assert!(is_progress(line), "{line:?}");
        }
    }

    #[test]
    fn percentages_and_bars_are_progress() {
        for line in [
            " 47%",
            "47% done",
            "[ 12%]",
            "100% complete",
            "[####################----]",
            "━━━━━━━━━━╾─────────────",
            "⠋ building",
        ] {
            assert!(is_progress(line), "{line:?}");
        }
    }

    #[test]
    fn content_that_merely_contains_a_number_is_not_progress() {
        // The failure that matters: dropping a line the reader needed because
        // it happened to mention a percentage or a verb.
        for line in [
            "error: 50% of tests failed",
            "warning: coverage dropped to 68%",
            "Compiling",
            "test result: ok. 74 passed",
            "   Finished `release` profile [optimized] target(s) in 2.43s",
            "assert_eq!(rate, 0.5) // 50% of the total",
        ] {
            assert!(!is_progress(line), "{line:?} must survive");
        }
    }

    #[test]
    fn an_empty_line_is_not_progress() {
        assert!(!is_progress(""));
        assert!(!is_progress("   "));
    }

    #[test]
    fn a_dropped_block_says_how_much_went() {
        let mut doc = doc_of(&["   Compiling a v1.0", "error: boom", " 42%"]);
        Progress.apply(&mut doc, &Ctx::default());

        assert!(!doc.blocks[0].kept());
        assert_eq!(doc.blocks[0].class, Class::Progress);
        assert_eq!(doc.blocks[0].elided.as_ref().unwrap().reason, "progress");
        assert_eq!(doc.blocks[0].elided.as_ref().unwrap().lines_removed, 1);

        assert!(doc.blocks[1].kept(), "the error survives");
        assert!(!doc.blocks[2].kept());
    }

    #[test]
    fn a_mixed_block_survives_whole() {
        // A block is dropped only if all of it is progress. One real line in the
        // block keeps the whole block, because splitting it would mean deciding
        // where the reader's context ends.
        let mut doc = Doc {
            blocks: vec![super::super::Block::new(vec![
                super::super::Line { text: "   Compiling a v1.0".into(), origin: 1 },
                super::super::Line { text: "error: boom".into(), origin: 2 },
            ])],
            source: super::super::Stream::Stdout,
            budget_exceeded: false,
        };
        Progress.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks[0].kept());
    }

    #[test]
    fn a_forced_block_is_not_dropped() {
        let mut doc = doc_of(&["   Compiling a v1.0"]);
        doc.blocks[0].force();
        Progress.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks[0].kept());
    }
}
