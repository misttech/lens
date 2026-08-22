// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Collapse repetition.
//!
//! The same warning emitted four hundred times tells the reader one thing, and
//! the four hundredth copy costs as much context as the first. Two shapes are
//! worth collapsing:
//!
//! * consecutive identical blocks — the same line or stanza, over and over;
//! * a repeated multi-block pattern, which is what a loop or a per-file check
//!   produces.
//!
//! What makes this safe is that the count survives. "×400" is the information
//! the four hundred copies carried, so nothing decision-relevant is lost — and
//! the copies are still in the store if the reader wants them.

use super::{Block, Class, Ctx, Doc, Keep, Kind, Line, Stage};

/// The dedupe stage.
#[derive(Debug, Clone, Copy)]
pub struct Dedupe;

impl Stage for Dedupe {
    fn name(&self) -> &'static str {
        "dedupe"
    }

    fn apply(&self, doc: &mut Doc, _ctx: &Ctx) {
        split_repeats(doc);
        collapse_consecutive(doc);
        collapse_repeated_windows(doc);
    }
}

/// Split repeating runs of lines inside a block into their own blocks.
///
/// The adapter has no reason to break a block apart where nothing changes
/// visually: a check that prints four lines per service, indented, for twelve
/// hundred services, is one block from its side. So the commonest repetition in
/// real output — a loop body, not a repeated line — arrives whole, and a
/// block-level comparison never sees it.
///
/// Windows from one line up to [`MAX_WINDOW`], shortest first, so a loop that
/// prints four lines collapses as four rather than as an accident of alignment.
/// Splitting here rather than deleting keeps every removal countable, and every
/// surviving line keeps the origin it was parsed with.
fn split_repeats(doc: &mut Doc) {
    /// A run has to repeat this many times before collapsing. Twice is a pair,
    /// and replacing one of two copies with a count saves nothing worth the
    /// reader's doubt.
    const MIN_REPEATS: usize = 3;
    /// The longest loop body worth looking for. Beyond this the search costs
    /// more than the repetition it would find.
    const MAX_WINDOW: usize = 12;

    let mut rebuilt: Vec<Block> = Vec::with_capacity(doc.blocks.len());

    for block in std::mem::take(&mut doc.blocks) {
        if block.keep == Keep::Drop || block.lines.len() < MIN_REPEATS {
            rebuilt.push(block);
            continue;
        }

        let (class, keep, kind) = (block.class, block.keep, block.kind.clone());
        let exact: Vec<String> = block.lines.iter().map(|l| l.text.clone()).collect();
        let normalized: Vec<String> = exact.iter().map(|t| normalize(t)).collect();
        let mut lines: Vec<Option<Line>> = block.lines.into_iter().map(Some).collect();

        let mut plain: Vec<Line> = Vec::new();
        let mut at = 0usize;

        while at < lines.len() {
            let Some((window, repeats)) =
                repeat_at(&exact, &normalized, at, MAX_WINDOW, MIN_REPEATS)
            else {
                plain.push(lines[at].take().expect("each line is taken once"));
                at += 1;
                continue;
            };

            // Everything before the repetition ends its own block.
            if !plain.is_empty() {
                rebuilt.push(rebuild(std::mem::take(&mut plain), class, keep, kind.clone()));
            }

            let take = |lines: &mut Vec<Option<Line>>, from: usize, count: usize| -> Vec<Line> {
                (from..from + count)
                    .map(|i| lines[i].take().expect("each line is taken once"))
                    .collect()
            };

            let mut survivor = rebuild(take(&mut lines, at, window), class, keep, kind.clone());
            annotate_block(&mut survivor, repeats);
            rebuilt.push(survivor);

            let copies = take(&mut lines, at + window, window * (repeats - 1));
            let mut dropped = rebuild(copies, class, keep, kind.clone());
            dropped.drop_with("dedupe");
            rebuilt.push(dropped);

            at += window * repeats;
        }

        if !plain.is_empty() {
            rebuilt.push(rebuild(plain, class, keep, kind));
        }
    }

    doc.blocks = rebuilt;
}

/// The shortest window starting at `at` that repeats at least `min_repeats`
/// times, and how many times it repeats.
///
/// A single line has to repeat *exactly*. Normalizing it would make `FAILED
/// auth_1`, `FAILED auth_2` and `FAILED auth_3` one line and a count — three
/// distinct facts replaced by a number that does not carry them. The same
/// flattening applied to four thousand records left one line reading `×4000`.
///
/// Longer windows keep the normalization, because that is what a loop body
/// needs: the timing and the counter change every iteration and mean nothing to
/// the comparison, while the shape around them is the repetition.
///
/// Either way, a window that reports a failure is never collapsed. Under any
/// doubt about whether two failures are the same failure, they are not.
fn repeat_at(
    exact: &[String],
    normalized: &[String],
    at: usize,
    max_window: usize,
    min_repeats: usize,
) -> Option<(usize, usize)> {
    for window in 1..=max_window.min((exact.len() - at) / min_repeats) {
        // Normalization is what lets a loop body match across iterations whose
        // timings and counters differ. It earns that only when the window has
        // shape to match — two or more lines that differ from each other. A
        // window whose lines are all the same shape is a single-line pattern
        // wearing a longer window, and flattening it turns distinct records
        // into one line and a count.
        let has_shape = normalized[at..at + window].iter().any(|l| l != &normalized[at]);

        let same = |a: usize, b: usize| -> bool {
            if has_shape {
                normalized[a..a + window] == normalized[b..b + window]
            } else {
                exact[a..a + window] == exact[b..b + window]
            }
        };

        let mut repeats = 1usize;
        while at + window * (repeats + 1) <= exact.len() && same(at, at + window * repeats) {
            repeats += 1;
        }

        if repeats >= min_repeats && !reports_failure(&exact[at..at + window]) {
            return Some((window, repeats));
        }
    }
    None
}

/// Does this window report a failure?
///
/// Two failures that look alike are still two failures, and a reader who is
/// shown one of them plus a count has been told the wrong thing.
fn reports_failure(window: &[String]) -> bool {
    matches!(super::classify::classify_text(&window.join("\n")), Class::Error | Class::Failure)
}

/// A block carrying the class and keep state of the one it came from.
fn rebuild(lines: Vec<Line>, class: Class, keep: Keep, kind: Kind) -> Block {
    let mut block = Block::new(lines);
    block.class = class;
    block.keep = keep;
    block.kind = kind;
    block
}

/// Drop consecutive blocks identical to the one before them, annotating the
/// survivor with how many there were.
fn collapse_consecutive(doc: &mut Doc) {
    let mut anchor: Option<usize> = None;
    let mut repeats = 0usize;

    for index in 0..doc.blocks.len() {
        let same_as_anchor = anchor.is_some_and(|a| {
            doc.blocks[a].lines.len() == doc.blocks[index].lines.len()
                && doc.blocks[a]
                    .lines
                    .iter()
                    .zip(&doc.blocks[index].lines)
                    .all(|(x, y)| x.text == y.text)
        });

        if same_as_anchor && doc.blocks[index].kept() {
            if doc.blocks[index].drop_with("dedupe") {
                repeats += 1;
            }
            continue;
        }

        if let (Some(a), 1..) = (anchor, repeats) {
            annotate(doc, a, repeats + 1);
        }
        anchor = doc.blocks[index].kept().then_some(index);
        repeats = 0;
    }

    if let (Some(a), 1..) = (anchor, repeats) {
        annotate(doc, a, repeats + 1);
    }
}

/// Find a repeated run of blocks and drop every copy after the first.
///
/// A rolling window over normalized block text, longest window first: a loop
/// that emits three blocks per iteration should collapse as three, not as one
/// block repeated three times out of phase.
fn collapse_repeated_windows(doc: &mut Doc) {
    const MAX_WINDOW: usize = 8;
    const MIN_REPEATS: usize = 3;

    let live: Vec<usize> = (0..doc.blocks.len()).filter(|i| doc.blocks[*i].kept()).collect();
    if live.len() < MIN_REPEATS * 2 {
        return;
    }

    let exact: Vec<String> = live.iter().map(|i| doc.blocks[*i].text()).collect();
    let text: Vec<String> = exact.iter().map(|t| normalize(t)).collect();
    // Normalized line by line, so a window can be asked whether the unit that
    // repeats has internal shape — which is the question, not whether one
    // iteration differs from the next.
    let norm_lines: Vec<Vec<String>> = live
        .iter()
        .map(|i| doc.blocks[*i].lines.iter().map(|l| normalize(&l.text)).collect())
        .collect();

    for window in (1..=MAX_WINDOW.min(live.len() / MIN_REPEATS)).rev() {
        let mut start = 0usize;
        while start + window * MIN_REPEATS <= live.len() {
            // Normalization is what lets a loop body match across iterations
            // whose counters differ. It earns that only when the repeating unit
            // has shape: two or more lines that differ from each other. A unit
            // that is one line, or several lines of one shape, is a single line
            // wearing a window — and flattening it turns distinct records into
            // one line and a count.
            let unit: Vec<&String> = norm_lines[start..start + window].iter().flatten().collect();
            let has_shape = unit.iter().any(|l| *l != unit[0]);

            let same = |a: usize, b: usize| -> bool {
                if has_shape {
                    text[a..a + window] == text[b..b + window]
                } else {
                    exact[a..a + window] == exact[b..b + window]
                }
            };

            let mut repeats = 1usize;
            while start + window * (repeats + 1) <= live.len()
                && same(start, start + window * repeats)
            {
                repeats += 1;
            }

            // And a window that reports a failure is never collapsed. Two
            // failures that look alike are still two failures — today they
            // survive only because context force-keeps them afterwards, which
            // is a rescue rather than a rule.
            if repeats >= MIN_REPEATS && !reports_failure(&exact[start..start + window]) {
                for copy in 1..repeats {
                    for offset in 0..window {
                        let index = live[start + window * copy + offset];
                        doc.blocks[index].drop_with("dedupe");
                    }
                }
                annotate(doc, live[start + window - 1], repeats);
                start += window * repeats;
            } else {
                start += 1;
            }
        }
    }
}

/// Append the repeat count to a block's last line.
///
/// The annotation is marked with the same `[lens:` prefix every other
/// announcement uses, so nothing Lens wrote can be mistaken for something the
/// command wrote.
fn annotate(doc: &mut Doc, index: usize, total: usize) {
    let Some(block) = doc.blocks.get_mut(index) else { return };
    annotate_block(block, total);
}

fn annotate_block(block: &mut Block, total: usize) {
    let Some(last) = block.lines.last_mut() else { return };
    if last.text.contains("[lens: ×") {
        return;
    }
    last.text.push_str(&format!("  [lens: ×{total}]"));
}

/// Strip variable detail so two runs of the same work compare equal.
///
/// Timings, addresses and counters differ on every iteration and mean nothing to
/// the comparison. Digits are the whole of that difference in practice, and
/// flattening them is cheaper than parsing.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_digits = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
                in_digits = true;
            }
        } else {
            in_digits = false;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::*;

    fn kept(doc: &Doc) -> Vec<String> {
        doc.blocks.iter().filter(|b| b.kept()).map(|b| b.text()).collect()
    }

    #[test]
    fn consecutive_duplicates_collapse_to_one_with_a_count() {
        let mut doc = doc_of(&["warn: x", "warn: x", "warn: x", "done"]);
        Dedupe.apply(&mut doc, &Ctx::default());

        assert_eq!(kept(&doc), vec!["warn: x  [lens: ×3]", "done"]);
        assert_eq!(doc.elided_line_count(), 2);
    }

    #[test]
    fn the_count_is_what_replaces_the_copies() {
        // The claim that makes this safe: the information in 400 copies is the
        // number 400, and it survives.
        let lines: Vec<&str> = std::iter::repeat_n("warning: unused", 400).collect();
        let mut doc = doc_of(&lines);
        Dedupe.apply(&mut doc, &Ctx::default());

        assert_eq!(kept(&doc).len(), 1);
        assert!(kept(&doc)[0].contains("×400"));
    }

    #[test]
    fn a_single_occurrence_is_not_annotated() {
        let mut doc = doc_of(&["one", "two", "three"]);
        Dedupe.apply(&mut doc, &Ctx::default());
        assert_eq!(kept(&doc), vec!["one", "two", "three"]);
    }

    #[test]
    fn non_consecutive_duplicates_both_survive() {
        // Position carries meaning: the same warning before and after a fix is
        // two facts, not one repeated.
        let mut doc = doc_of(&["warn: x", "other", "warn: x"]);
        Dedupe.apply(&mut doc, &Ctx::default());
        assert_eq!(kept(&doc), vec!["warn: x", "other", "warn: x"]);
    }

    #[test]
    fn lines_that_differ_only_by_a_number_are_not_one_line() {
        // Normalizing a single line made four thousand distinct records read as
        // one plus `×4000`. The count is not the information the records
        // carried, and no marker makes it so.
        let lines: Vec<String> =
            (1..=20).map(|i| format!("processed record {i} in {}us", i * 37)).collect();
        let mut doc = Doc {
            blocks: vec![Block::new(
                lines
                    .iter()
                    .enumerate()
                    .map(|(i, text)| Line { text: text.clone(), origin: i + 1 })
                    .collect(),
            )],
            source: super::super::Stream::Stdout,
            budget_exceeded: false,
        };
        Dedupe.apply(&mut doc, &Ctx::default());

        let survivors: usize = doc.blocks.iter().filter(|b| b.kept()).map(|b| b.lines.len()).sum();
        assert_eq!(survivors, 20, "every record is its own fact");
    }

    #[test]
    fn repeated_failures_are_never_collapsed() {
        // Three failing tests that differ only by an index are three failures.
        // Showing one and a count tells the reader something untrue about what
        // broke, which is the worst thing this tool can do.
        let mut doc =
            doc_of(&["FAILED tests::auth_1", "FAILED tests::auth_2", "FAILED tests::auth_3"]);
        Dedupe.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks.iter().all(|b| b.kept()), "{:?}", kept(&doc));
        assert!(!kept(&doc).iter().any(|t| t.contains("[lens:")), "and no count is claimed");
    }

    #[test]
    fn identical_lines_still_collapse() {
        // The fix for the two tests above must not cost the case dedupe exists
        // for: the same line, verbatim, many times.
        let mut doc = doc_of(&["warning: unused", "warning: unused", "warning: unused"]);
        Dedupe.apply(&mut doc, &Ctx::default());
        assert_eq!(kept(&doc).len(), 1);
        assert!(kept(&doc)[0].contains("×3"));
    }

    #[test]
    fn distinct_blocks_are_not_one_block_and_a_count() {
        // The block-level twin of the line-level rule. Eight thousand blocks
        // differing only by an index collapsed to two, and `×4000` does not
        // carry what the other 3,999 said.
        let lines: Vec<String> = (1..=30).map(|i| format!("line {i} unique content")).collect();
        let mut doc = doc_of(&lines.iter().map(String::as_str).collect::<Vec<_>>());
        Dedupe.apply(&mut doc, &Ctx::default());
        assert_eq!(doc.blocks.iter().filter(|b| b.kept()).count(), 30);
    }

    #[test]
    fn repeated_failing_blocks_are_never_collapsed() {
        // These survive today only because context force-keeps them after the
        // fact. Dedupe has to refuse on its own: a rescue that happens to run
        // later is not a rule.
        let mut doc = Doc {
            blocks: (1..=6)
                .map(|i| {
                    Block::new(vec![
                        Line { text: format!("FAILED tests::case_{i}"), origin: i * 2 - 1 },
                        Line { text: format!("  expected 200 got 50{i}"), origin: i * 2 },
                    ])
                })
                .collect(),
            source: super::super::Stream::Stdout,
            budget_exceeded: false,
        };
        Dedupe.apply(&mut doc, &Ctx::default());

        assert!(doc.blocks.iter().all(|b| b.kept()), "every failure stays: {:?}", kept(&doc));
        assert!(
            !kept(&doc).iter().any(|t| t.contains("[lens: ×")),
            "and none of them claims to stand for the others"
        );
    }

    #[test]
    fn a_loop_body_inside_one_block_collapses() {
        // Found by the retention benchmark, on its first task: a check printing
        // four lines per service for twelve hundred services is one block, and
        // comparing whole blocks never saw the repetition. 191 KB of output
        // survived filtering untouched.
        let mut lines: Vec<String> = Vec::new();
        for i in 1..=200 {
            lines.push(format!("   Checking service {i} ... ok"));
            lines.push(format!("     endpoint https://svc-{i}.internal:8443 reachable"));
            lines.push("     tls certificate valid until 2027-01-01".to_string());
            lines.push(format!("     health probe 200 in {}ms", i % 90 + 4));
        }
        lines.push("required configuration key: retry_after_ms".to_string());

        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut doc = Doc {
            blocks: vec![Block::new(
                refs.iter()
                    .enumerate()
                    .map(|(i, text)| Line { text: (*text).to_string(), origin: i + 1 })
                    .collect(),
            )],
            source: super::super::Stream::Stdout,
            budget_exceeded: false,
        };
        Dedupe.apply(&mut doc, &Ctx::default());

        let survivors: usize = doc.blocks.iter().filter(|b| b.kept()).map(|b| b.lines.len()).sum();
        assert!(survivors <= 6, "one iteration and the tail should survive, got {survivors}");

        // The line that matters is the last one, and no amount of collapsing may
        // reach it.
        let text: String =
            doc.blocks.iter().filter(|b| b.kept()).map(|b| b.text()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("retry_after_ms"), "{text}");
        assert!(text.contains("×200"), "the count replaces the copies: {text}");
    }

    #[test]
    fn a_repeated_pattern_collapses_as_a_unit() {
        // A per-file loop emitting two lines each: three iterations collapse to
        // one, counted — not to one line repeated six times.
        let mut doc = doc_of(&[
            "checking file 1",
            "  ok",
            "checking file 2",
            "  ok",
            "checking file 3",
            "  ok",
        ]);
        Dedupe.apply(&mut doc, &Ctx::default());

        let survivors = kept(&doc);
        assert_eq!(survivors.len(), 2, "one iteration survives: {survivors:?}");
        assert!(survivors[1].contains("×3"), "{survivors:?}");
    }

    #[test]
    fn varying_numbers_do_not_defeat_pattern_matching() {
        // Timings differ every iteration and mean nothing to the comparison.
        let mut doc = doc_of(&[
            "run 1",
            "  took 12ms",
            "run 2",
            "  took 47ms",
            "run 3",
            "  took 8ms",
            "run 4",
            "  took 91ms",
        ]);
        Dedupe.apply(&mut doc, &Ctx::default());
        assert!(kept(&doc).len() < 8, "repeats should collapse: {:?}", kept(&doc));
    }

    #[test]
    fn a_pair_of_identical_lines_survives_intact() {
        // The bug this caught: a run below the collapse threshold had its
        // copies discarded — content gone with no count and no marker.
        let mut doc = Doc {
            blocks: vec![Block::new(vec![
                Line { text: "warning: a".into(), origin: 1 },
                Line { text: "warning: a".into(), origin: 2 },
            ])],
            source: super::super::Stream::Stdout,
            budget_exceeded: false,
        };
        Dedupe.apply(&mut doc, &Ctx::default());

        let kept_lines: usize = doc.blocks.iter().filter(|b| b.kept()).map(|b| b.lines.len()).sum();
        assert_eq!(kept_lines, 2, "both copies stay: {:?}", kept(&doc));
    }

    #[test]
    fn two_repeats_are_left_alone() {
        // Below the threshold. Collapsing a pair saves one block and costs the
        // reader the second copy's detail, which is a bad trade.
        let mut doc = doc_of(&["a", "b", "a", "b"]);
        Dedupe.apply(&mut doc, &Ctx::default());
        assert_eq!(kept(&doc).len(), 4);
    }

    #[test]
    fn a_forced_block_survives_deduplication() {
        let mut doc = doc_of(&["same", "same", "same"]);
        doc.blocks[2].force();
        Dedupe.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks[2].kept());
    }

    #[test]
    fn annotation_is_marked_as_ours() {
        // Nothing Lens writes may be mistaken for something the command wrote.
        let mut doc = doc_of(&["x", "x", "x"]);
        Dedupe.apply(&mut doc, &Ctx::default());
        assert!(kept(&doc)[0].contains("[lens:"));
    }

    #[test]
    fn annotating_twice_does_not_stack() {
        let mut doc = doc_of(&["x", "x", "x"]);
        Dedupe.apply(&mut doc, &Ctx::default());
        Dedupe.apply(&mut doc, &Ctx::default());
        assert_eq!(kept(&doc)[0].matches("[lens:").count(), 1);
    }

    #[test]
    fn normalization_flattens_digit_runs() {
        assert_eq!(normalize("took 1234ms"), "took #ms");
        assert_eq!(normalize("0x7f8a"), "#x#f#a");
        assert_eq!(normalize("no digits"), "no digits");
    }
}
