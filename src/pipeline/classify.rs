// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Assign a class to every block, and enforce the failure floor.
//!
//! The floor is the important half. A command that exited non-zero produced a
//! failure somewhere in its output, and a filtered view that does not show it is
//! the worst thing this tool can produce: the reader concludes the command
//! succeeded and acts on that. So when the child failed and nothing was
//! recognized as a failure, the tail of the output is force-kept — an unhelpful
//! view is recoverable, a misleading one is not.

use super::{Class, Ctx, Doc, Keep, Stage, Stream};

/// The classify stage.
#[derive(Debug, Clone, Copy)]
pub struct Classify;

impl Stage for Classify {
    fn name(&self) -> &'static str {
        "classify"
    }

    fn apply(&self, doc: &mut Doc, ctx: &Ctx) {
        for block in &mut doc.blocks {
            // The progress stage has already spoken for these.
            if block.class == Class::Progress {
                continue;
            }
            block.class = classify_text(&block.text());
        }

        if ctx.exit_code != 0 {
            enforce_failure_floor(doc);
        }
    }
}

/// How many trailing blocks to rescue when a failure went unrecognized.
const FLOOR_BLOCKS: usize = 5;

/// Words that mean something failed.
const FAILURE_WORDS: &[&str] = &[
    "error", "errors", "failed", "failure", "failures", "panic", "panicked", "fatal", "abort",
    "aborted",
];

/// Words that name a reported failure rather than an error message.
const TEST_FAILURE_WORDS: &[&str] = &["failed", "failures", "failing", "not ok"];

/// Words that mean something is worth knowing but not acting on.
const WARNING_WORDS: &[&str] = &["warning", "warnings", "warn", "deprecated", "deprecation"];

/// Words that carry no information on their own.
const NOISE_WORDS: &[&str] = &["", "---", "===", "***"];

/// Classify one block's text.
///
/// Judged line by line, and a failure word only counts where a diagnostic would
/// put it: at the start of a line, immediately before a `:` or `[`, or in one of
/// the fixed shapes a runner reports. Prose that happens to discuss errors —
/// a commit message, a README, this file's own comments — is not a failure, and
/// treating it as one is not a harmless over-count: it decides what survives
/// budget pressure and what a level-1 view is allowed to drop.
pub fn classify_text(text: &str) -> Class {
    let mut class = Class::Info;

    for line in text.lines() {
        let lower = line.to_ascii_lowercase();

        if is_failure_line(&lower) {
            // `error` in a message about a test that failed is a Failure;
            // standing alone it is an Error. Both outrank everything below, so
            // the first one found settles the block.
            return if contains_word(&lower, TEST_FAILURE_WORDS) && lower.contains("test") {
                Class::Failure
            } else {
                Class::Error
            };
        }
        if is_warning_line(&lower) {
            class = Class::Warning;
        }
    }

    if class == Class::Info {
        let trimmed = text.trim();
        if NOISE_WORDS.contains(&trimmed)
            || (!trimmed.is_empty() && trimmed.chars().all(|c| !c.is_alphanumeric()))
        {
            return Class::Noise;
        }
    }

    class
}

/// Does this line report a failure, as opposed to mentioning one?
fn is_failure_line(lower: &str) -> bool {
    if lower.contains("panicked at")
        || lower.contains("traceback (most recent call last)")
        || lower.contains("stack trace")
    {
        return true;
    }
    // The last line of a Python traceback, and the one a reader actually needs:
    // `ValueError: ...`, `jinja2.exceptions.TemplateNotFound: ...`. The failure
    // word is buried in a dotted type name, so no word-boundary rule finds it.
    // What marks the line is a leading type naming an error or an exception,
    // followed by the colon that introduces the message.
    let first = lower.split_whitespace().next().unwrap_or("");
    if first.ends_with(':') && (first.contains("error") || first.contains("exception")) {
        return true;
    }

    reports(lower, FAILURE_WORDS)
}

/// The same question for warnings.
fn is_warning_line(lower: &str) -> bool {
    reports(lower, WARNING_WORDS)
}

/// Is one of `words` used here as a report rather than as a noun in a sentence?
///
/// Three shapes, which between them cover every runner and compiler worth
/// filtering:
///
/// * at the start of the line — `error: mismatched types`, `FAILED: 3 tests`
/// * immediately before punctuation that opens a detail — `error[E0308]`
/// * counted — `2 errors emitted`, `1 failed`
fn reports(lower: &str, words: &[&str]) -> bool {
    let trimmed = lower.trim_start();

    words.iter().any(|word| {
        // At the start of the line.
        if trimmed.strip_prefix(word).is_some_and(|rest| !starts_alnum(rest)) {
            return true;
        }

        lower.match_indices(word).any(|(at, _)| {
            let after = lower[at + word.len()..].chars().next();
            let before_alnum = lower[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric());
            if before_alnum {
                return false;
            }

            // Opening a detail: `error[E0308]`, `error:`, `failure(3)`.
            if matches!(after, Some(':') | Some('[') | Some('(')) {
                return true;
            }

            // Counted: the word is preceded by a number.
            let head = lower[..at].trim_end();
            head.chars().next_back().is_some_and(|c| c.is_ascii_digit())
        })
    })
}

fn starts_alnum(rest: &str) -> bool {
    rest.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Does `lower` contain any of `words` as a whole word?
fn contains_word(lower: &str, words: &[&str]) -> bool {
    words.iter().any(|word| {
        lower.match_indices(word).any(|(at, _)| {
            let before = lower[..at].chars().next_back();
            let after = lower[at + word.len()..].chars().next();
            let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
            boundary(before) && boundary(after)
        })
    })
}

/// Force-keep the tail when the child failed and nothing looks like a failure.
///
/// Deliberately blunt: it does not try to find the failure, only to guarantee
/// the reader sees the end of the output, which is where a failure almost always
/// is. Being wrong here costs some tokens; not doing it costs correctness.
fn enforce_failure_floor(doc: &mut Doc) {
    if doc.has_failure() {
        return;
    }
    // stderr is where a failing command usually explains itself; when a
    // document has none of it, the tail of stdout is the best available answer.
    let rescue = if doc.source == Stream::Stderr { FLOOR_BLOCKS } else { FLOOR_BLOCKS.min(3) };

    for block in doc.blocks.iter_mut().rev().take(rescue) {
        if block.class == Class::Progress {
            continue;
        }
        block.keep = Keep::Force;
        block.elided = None;
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::*;

    #[test]
    fn failures_are_recognized() {
        for text in [
            "error[E0308]: mismatched types",
            "FAILED: 3 tests did not pass",
            "thread 'main' panicked at src/main.rs:4",
            "fatal: not a git repository",
            "Traceback (most recent call last):",
            "ValueError: invalid literal for int()",
            "jinja2.exceptions.TemplateNotFound: page.html",
        ] {
            assert!(
                matches!(classify_text(text), Class::Error | Class::Failure),
                "{text:?} classified {:?}",
                classify_text(text)
            );
        }
    }

    #[test]
    fn warnings_are_not_failures() {
        // The shapes compilers actually emit. A sentence merely containing
        // "deprecated" is prose, and is classified as such.
        for text in ["warning: unused variable `x`", "warning: use of deprecated function"] {
            assert_eq!(classify_text(text), Class::Warning, "{text:?}");
        }
    }

    #[test]
    fn matching_is_anchored_at_word_boundaries() {
        // The false positive that would otherwise make everything an error.
        assert_eq!(classify_text("the terror of it"), Class::Info);
        assert_eq!(classify_text("unerroring"), Class::Info);
        assert_eq!(classify_text("warningless"), Class::Info);
        // ...and the true positive it must not cost.
        assert_eq!(classify_text("error: compilation failed"), Class::Error);
        assert_eq!(classify_text("2 errors emitted"), Class::Error);
    }

    #[test]
    fn prose_about_errors_is_not_a_failure() {
        // Found by running `lens git show` over this repository: a commit
        // message that discusses errors was classified as fourteen failures,
        // and level 0 reported "14 failing" for a command that exited 0.
        for text in [
            "classify assigns severity and enforces the floor: a non-zero exit",
            "a filtered view of a failed command that reads as success is the",
            "worst output this tool can produce",
            "an error without what it was attempting is not actionable",
            "the failure that matters is dropping a line the reader needed",
            "Fixes the error handling in the parser",
        ] {
            assert_eq!(classify_text(text), Class::Info, "{text:?}");
        }
    }

    #[test]
    fn the_more_serious_signal_wins() {
        // Under budget pressure this decides which block survives, so a block
        // that mentions both has to be treated as the failure it contains.
        assert_eq!(classify_text("warning: x\nerror: y"), Class::Error);
    }

    #[test]
    fn separators_are_noise() {
        assert_eq!(classify_text("---------"), Class::Noise);
        assert_eq!(classify_text("========="), Class::Noise);
        assert_eq!(classify_text("ordinary output"), Class::Info);
    }

    #[test]
    fn progress_classification_is_not_overwritten() {
        let mut doc = doc_of(&["   Compiling lens v0.1.0"]);
        doc.blocks[0].class = Class::Progress;
        Classify.apply(&mut doc, &Ctx::default());
        assert_eq!(doc.blocks[0].class, Class::Progress);
    }

    #[test]
    fn a_failing_command_always_shows_something() {
        // The bug this exists to prevent: exit code 1, and a view with no sign
        // of failure anywhere in it.
        let mut doc = doc_of(&["setting up", "running", "cleaning up", "goodbye"]);
        let ctx = Ctx { exit_code: 1, ..Ctx::default() };
        Classify.apply(&mut doc, &ctx);

        let forced = doc.blocks.iter().filter(|b| b.keep == Keep::Force).count();
        assert!(forced > 0, "a failing command must force-keep something");
        assert!(doc.blocks.last().unwrap().keep == Keep::Force, "the tail is what is kept");
    }

    #[test]
    fn the_floor_does_not_fire_when_a_failure_is_visible() {
        // Nothing to rescue: the failure is already in the view, so forcing the
        // tail would spend tokens on nothing.
        let mut doc = doc_of(&["ok", "error: boom", "ok"]);
        let ctx = Ctx { exit_code: 1, ..Ctx::default() };
        Classify.apply(&mut doc, &ctx);
        assert!(doc.blocks.iter().all(|b| b.keep != Keep::Force));
    }

    #[test]
    fn the_floor_does_not_fire_on_success() {
        let mut doc = doc_of(&["all good", "done"]);
        Classify.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks.iter().all(|b| b.keep != Keep::Force));
    }

    #[test]
    fn the_floor_rescues_a_block_progress_had_dropped() {
        // A failing command whose last real line was dropped by an earlier
        // stage still has to show it.
        let mut doc = doc_of(&["step one", "quietly wrong"]);
        doc.blocks[1].drop_with("dedupe");
        let ctx = Ctx { exit_code: 2, ..Ctx::default() };
        Classify.apply(&mut doc, &ctx);

        assert!(doc.blocks[1].kept(), "the tail is back in the view");
        assert!(doc.blocks[1].elided.is_none(), "and is no longer reported as removed");
    }

    #[test]
    fn stderr_gets_a_deeper_rescue_than_stdout() {
        let failing = Ctx { exit_code: 1, ..Ctx::default() };
        let lines = &["a", "b", "c", "d", "e", "f", "g"];

        let mut out = doc_of(lines);
        Classify.apply(&mut out, &failing);
        let mut err = doc_of(lines);
        err.source = Stream::Stderr;
        Classify.apply(&mut err, &failing);

        let forced = |doc: &Doc| doc.blocks.iter().filter(|b| b.keep == Keep::Force).count();
        assert!(forced(&err) > forced(&out), "stderr is where a failure explains itself");
    }
}
