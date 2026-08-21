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

use super::{Block, Class, Ctx, Doc, Keep, Line, Stage};

/// The dedupe stage.
#[derive(Debug, Clone, Copy)]
pub struct Dedupe;

impl Stage for Dedupe {
    fn name(&self) -> &'static str {
        "dedupe"
    }

    fn apply(&self, doc: &mut Doc, _ctx: &Ctx) {
        split_repeated_lines(doc);
        collapse_consecutive(doc);
        collapse_repeated_windows(doc);
    }
}

/// Split runs of identical lines inside a block into their own blocks.
///
/// The adapter has no reason to break a run of identical lines apart — from its
/// side they are one stanza — so the commonest repetition of all, the same
/// warning printed a hundred times with nothing between, arrives as a single
/// block. Splitting it here lets the same marking machinery handle it, which
/// keeps every removal countable and every line's origin intact.
fn split_repeated_lines(doc: &mut Doc) {
    /// Runs longer than this collapse; a pair stays whole, because replacing
    /// one of two copies with a count saves nothing worth the reader's doubt.
    const MIN_RUN: usize = 2;

    if !doc.blocks.iter().any(has_repeated_lines) {
        return;
    }

    let mut rebuilt: Vec<Block> = Vec::with_capacity(doc.blocks.len());

    for block in std::mem::take(&mut doc.blocks) {
        if block.keep == Keep::Drop || !has_repeated_lines(&block) {
            rebuilt.push(block);
            continue;
        }

        let class = block.class;
        let keep = block.keep;
        let mut plain: Vec<Line> = Vec::new();
        let mut lines = block.lines.into_iter().peekable();

        while let Some(line) = lines.next() {
            let mut copies: Vec<Line> = Vec::new();
            while lines.peek().is_some_and(|next| next.text == line.text) {
                copies.push(lines.next().expect("peeked"));
            }

            if copies.len() < MIN_RUN {
                // Below the threshold: the run stays whole. The copies go back
                // with it — dropping them here would remove content with no
                // count and no marker, which is the one thing no stage may do.
                plain.push(line);
                plain.extend(copies);
                continue;
            }

            // Everything gathered so far ends its own block, then the survivor
            // and its copies each get one.
            if !plain.is_empty() {
                rebuilt.push(rebuild(std::mem::take(&mut plain), class, keep));
            }

            let total = copies.len() + 1;
            let mut survivor = rebuild(vec![line], class, keep);
            annotate_block(&mut survivor, total);
            rebuilt.push(survivor);

            let mut dropped = rebuild(copies, class, keep);
            dropped.drop_with("dedupe");
            rebuilt.push(dropped);
        }

        if !plain.is_empty() {
            rebuilt.push(rebuild(plain, class, keep));
        }
    }

    doc.blocks = rebuilt;
}

/// Does this block contain two or more identical lines in a row?
fn has_repeated_lines(block: &Block) -> bool {
    block.lines.windows(2).any(|pair| pair[0].text == pair[1].text)
}

/// A block carrying the class and keep state of the one it came from.
fn rebuild(lines: Vec<Line>, class: Class, keep: Keep) -> Block {
    let mut block = Block::new(lines);
    block.class = class;
    block.keep = keep;
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

    let text: Vec<String> = live.iter().map(|i| normalize(&doc.blocks[*i].text())).collect();

    for window in (2..=MAX_WINDOW.min(live.len() / MIN_REPEATS)).rev() {
        let mut start = 0usize;
        while start + window * MIN_REPEATS <= live.len() {
            let pattern = &text[start..start + window];
            let mut repeats = 1usize;
            while start + window * (repeats + 1) <= live.len()
                && text[start + window * repeats..start + window * (repeats + 1)] == *pattern
            {
                repeats += 1;
            }

            if repeats >= MIN_REPEATS {
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
