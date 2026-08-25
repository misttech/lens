// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Collapse diagnostics that report one cause.
//!
//! A wrong field type is reported once per use site. A hundred and fifty
//! diagnostics, one edit, and every one of them says the same sentence — and
//! the reader who has seen the first has seen all of them.
//!
//! [`super::dedupe`] cannot do this. Those blocks are not repetition in the
//! sense that stage means: each names a different line, quotes a different
//! source excerpt, and carries a caret run whose length follows the width of
//! the literal underneath it. They are line-different and cause-identical, so
//! the key here is the message alone.
//!
//! That is also the risk. Two unrelated errors that happen to open with the
//! same sentence are grouped, and the second is elided behind a marker that
//! says how many followed. The alternative measured worse: a hundred and fifty
//! copies of one sentence is what the reader was already being handed, and the
//! full text stays in the store either way.

use std::collections::{HashMap, HashSet};

use super::dedupe::normalize;
use super::{Block, Class, Ctx, Doc, Line, Stage};

/// Flatten digits, but leave anything path-shaped alone.
///
/// `mod_0.js` and `mod_1.js` are two files. Flattening their digits makes them
/// one key, and grouping on it reports one file where thirty were named — which
/// is what happened: a 96% reduction that kept one file of thirty. A token
/// carrying a slash or an extension is a place, not a counter.
fn normalize_sites(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(
            |token| {
                if looks_like_path(token.trim()) { token.to_string() } else { normalize(token) }
            },
        )
        .collect()
}

fn looks_like_path(token: &str) -> bool {
    if token.contains('/') {
        return true;
    }
    // `name.ext`, where the extension is short and alphabetic: mod_0.js, a.rs.
    token.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=4).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphabetic())
    })
}

/// Below this a group is not a cascade. Two of the same message is a pair, and
/// a marker in place of the second one saves nothing worth the sentence.
const MIN_GROUP: usize = 3;

/// The cause-grouping stage.
#[derive(Debug, Clone, Copy)]
pub struct Cause;

impl Stage for Cause {
    fn name(&self) -> &'static str {
        "cause"
    }

    fn apply(&self, doc: &mut Doc, _ctx: &Ctx) {
        for block in &mut doc.blocks {
            group_within(block);
        }

        let keys: Vec<Option<String>> = doc.blocks.iter().map(key_for).collect();

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for key in keys.iter().flatten() {
            *counts.entry(key.as_str()).or_default() += 1;
        }

        // The first block of each group is the one that stays. It is the one a
        // reader would have read anyway, and keeping it is what makes this an
        // elision rather than a deletion of the failure.
        let mut seen: HashSet<&str> = HashSet::new();
        for (index, key) in keys.iter().enumerate() {
            let Some(key) = key.as_deref() else { continue };
            if counts.get(key).copied().unwrap_or(0) < MIN_GROUP {
                continue;
            }
            if seen.insert(key) {
                continue;
            }
            doc.blocks[index].drop_with("cause");
        }
    }
}

/// Collapse repeated reports inside one block.
///
/// Whether a hundred findings arrive as a hundred blocks or as one is a
/// question about the producer's blank lines, not about the findings. rustc
/// separates its diagnostics and tsc does not, so the same cascade is block
/// work in one and line work in the other, and grouping only blocks would be
/// grouping only the tools that happen to double-space.
///
/// [`super::dedupe`] will not do this either: it refuses to collapse a line
/// that reports a failure, by a deliberate rule that keeps a failing command's
/// output intact. The first report of each cause survives here for the same
/// reason.
fn group_within(block: &mut Block) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in &block.lines {
        if let Some(key) = line_key(&line.text) {
            *counts.entry(key).or_default() += 1;
        }
    }
    if !counts.values().any(|count| *count >= MIN_GROUP) {
        return;
    }

    // Counted per cause across the whole block rather than per run of adjacent
    // lines. tsc interleaves two findings per file, so a running count resets on
    // every other line and reports two of thirty.
    let mut seen: HashSet<String> = HashSet::new();
    let mut kept: Vec<Line> = Vec::with_capacity(block.lines.len());

    for mut line in std::mem::take(&mut block.lines) {
        match line_key(&line.text).filter(|key| counts[key] >= MIN_GROUP) {
            Some(key) => {
                if seen.insert(key.clone()) {
                    // The first report carries the count for all of them.
                    line.text.push_str(&format!("  [lens: ×{}]", counts[&key]));
                    kept.push(line);
                }
            }
            None => kept.push(line),
        }
    }

    block.lines = kept;
}

/// The cause a single line reports, or `None` when it does not report one.
///
/// A line is a report when it opens with a diagnostic's location, or when
/// classification alone calls it one. Ordinary output is never grouped here:
/// two identical lines of program output are [`super::dedupe`]'s business.
fn line_key(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();

    // A line that names its own file keys on that file as well as its message.
    // The same cause at thirty sites in one file is a cascade; the same cause
    // in thirty different files is thirty files a reader has to visit, and
    // flattening the paths into one takes twenty-nine of them out of the view.
    if let Some((path, rest)) = super::classify::split_location(&lower) {
        return Some(format!("{path}\u{1}{}", normalize_sites(rest)));
    }

    matches!(super::classify::classify_text(text), Class::Error | Class::Failure | Class::Warning)
        .then(|| normalize_sites(text))
}

/// The cause a block reports, or `None` when it does not report one.
///
/// Ordinary output is never grouped: two identical lines of program output are
/// [`super::dedupe`]'s business, and collapsing them here would elide content
/// nobody classified as a diagnostic.
fn key_for(block: &Block) -> Option<String> {
    // A reported failure names a thing that failed. Six tests failing the same
    // way are six failures, and a view saying one of them failed is wrong in
    // the direction this tool exists to avoid.
    if !matches!(block.class, Class::Error | Class::Warning) {
        return None;
    }

    // Whatever file the block names is part of its identity, wherever in the
    // block it says so. Without this, one fault in each of fifty files
    // collapses to one line naming one file.
    let site = block.lines.iter().find_map(|line| {
        let lower = line.text.to_ascii_lowercase();
        super::classify::arrow_location(&lower)
            .map(str::to_string)
            .or_else(|| super::classify::split_location(&lower).map(|(path, _)| path.to_string()))
    });

    // Digits carry the line number, the column, and the counter in the
    // identifier — everything that differs between two reports of one cause.
    let message = normalize_sites(&block.lines.first()?.text);
    Some(match site {
        Some(path) => format!("{path}\u{1}{message}"),
        None => message,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::super::{Keep, classify};
    use super::*;

    /// Classify then group, which is the order the pipeline uses.
    fn run(lines: &[&str]) -> Doc {
        let mut doc = doc_of(lines);
        let ctx = Ctx::default();
        classify::Classify.apply(&mut doc, &ctx);
        Cause.apply(&mut doc, &ctx);
        doc
    }

    fn kept(doc: &Doc) -> Vec<String> {
        doc.blocks.iter().filter(|b| b.kept()).map(|b| b.text()).collect()
    }

    #[test]
    fn a_cascade_keeps_one_and_elides_the_rest() {
        let doc = run(&[
            "error: mismatched types at 11",
            "error: mismatched types at 24",
            "error: mismatched types at 37",
            "error: mismatched types at 50",
        ]);
        assert_eq!(kept(&doc).len(), 1);
        assert_eq!(kept(&doc)[0], "error: mismatched types at 11");
        assert_eq!(doc.blocks.iter().filter(|b| !b.kept()).count(), 3);
    }

    #[test]
    fn the_survivor_is_the_first() {
        // Position is information: the first report is the one whose context
        // the reader is most likely to want.
        let doc = run(&["error: broken at 3", "error: broken at 9", "error: broken at 27"]);
        assert_eq!(kept(&doc)[0], "error: broken at 3");
    }

    #[test]
    fn a_pair_is_left_alone() {
        // Below the threshold a marker costs more than the line it replaces.
        let doc = run(&["error: broken at 3", "error: broken at 9"]);
        assert_eq!(kept(&doc).len(), 2);
    }

    #[test]
    fn different_causes_are_not_grouped() {
        let doc = run(&[
            "error: mismatched types at 3",
            "error: no method named send at 9",
            "error: mismatched types at 27",
            "error: no method named send at 44",
        ]);
        // Two groups of two: both below the threshold, so nothing is elided.
        assert_eq!(kept(&doc).len(), 4);
    }

    #[test]
    fn ordinary_output_is_never_grouped() {
        // Three identical lines of program output are dedupe's business. This
        // stage elides diagnostics, and only diagnostics.
        let doc = run(&["building thing 1", "building thing 2", "building thing 3"]);
        assert_eq!(kept(&doc).len(), 3);
    }

    #[test]
    fn an_elision_records_its_reason() {
        // The marker's wording is chosen from this, so an unlabelled drop would
        // read as a generic one.
        let doc = run(&["error: broken at 3", "error: broken at 9", "error: broken at 27"]);
        let dropped: Vec<_> = doc.blocks.iter().filter(|b| !b.kept()).collect();
        assert_eq!(dropped.len(), 2);
        for block in dropped {
            assert_eq!(block.keep, Keep::Drop);
            assert_eq!(block.elided.as_ref().unwrap().reason, "cause");
        }
    }

    #[test]
    fn a_forced_block_survives_grouping() {
        // Force outranks every stage that wants content gone. Whatever insisted
        // a block stay does not lose to a later count of its message.
        let mut doc = doc_of(&["error: broken at 3", "error: broken at 9", "error: broken at 27"]);
        let ctx = Ctx::default();
        classify::Classify.apply(&mut doc, &ctx);
        doc.blocks[2].force();
        Cause.apply(&mut doc, &ctx);
        assert!(doc.blocks[2].kept());
    }

    #[test]
    fn the_failure_is_still_visible() {
        // The property that matters: however large the cascade, the view still
        // reports the failure.
        let mut lines: Vec<String> =
            (0..200).map(|n| format!("error[E0308]: mismatched types at {n}")).collect();
        lines.insert(0, "running the build".to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let doc = run(&refs);
        assert!(kept(&doc).iter().any(|text| text.contains("mismatched types")));
    }

    /// A single block holding `lines`, which is what flat tool output parses to.
    fn flat(lines: &[&str]) -> Block {
        Block::new(
            lines
                .iter()
                .enumerate()
                .map(|(index, text)| Line { text: (*text).to_string(), origin: index + 1 })
                .collect(),
        )
    }

    #[test]
    fn reports_inside_one_block_are_grouped() {
        // Whether findings arrive as blocks or as lines is a question about the
        // producer's blank lines. tsc does not print any.
        let mut block = flat(&[
            "mod.ts(2,9): error TS2322: bad type",
            "mod.ts(9,4): error TS2322: bad type",
            "mod.ts(14,7): error TS2322: bad type",
        ]);
        group_within(&mut block);
        assert_eq!(block.lines.len(), 1);
        assert_eq!(block.lines[0].text, "mod.ts(2,9): error TS2322: bad type  [lens: ×3]");
    }

    #[test]
    fn interleaved_causes_are_counted_separately() {
        // The bug this pins: two findings per file, alternating, and a count
        // that resets on every other line reports two of thirty.
        let mut lines = Vec::new();
        for n in 0..12 {
            lines.push(format!("mod.ts({n},9): error TS2322: bad type"));
            lines.push(format!("mod.ts({n},4): error TS2339: no such property"));
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut block = flat(&refs);
        group_within(&mut block);

        assert_eq!(block.lines.len(), 2);
        assert!(block.lines[0].text.ends_with("[lens: ×12]"), "{}", block.lines[0].text);
        assert!(block.lines[1].text.ends_with("[lens: ×12]"), "{}", block.lines[1].text);
    }

    #[test]
    fn ordinary_lines_in_a_block_are_left_alone() {
        // Only reports are grouped. Everything else in the block is untouched,
        // in its original order.
        let mut block = flat(&[
            "running 3 checks",
            "a.rs:1:1: error: broken",
            "a.rs:2:1: error: broken",
            "a.rs:3:1: error: broken",
            "done",
        ]);
        group_within(&mut block);
        assert_eq!(block.lines.len(), 3);
        assert_eq!(block.lines[0].text, "running 3 checks");
        assert!(block.lines[1].text.ends_with("[lens: ×3]"));
        assert_eq!(block.lines[2].text, "done");
    }

    #[test]
    fn a_pair_inside_a_block_is_left_alone() {
        let mut block = flat(&["a.rs:1:1: error: broken", "a.rs:2:1: error: broken"]);
        group_within(&mut block);
        assert_eq!(block.lines.len(), 2);
    }

    #[test]
    fn grouping_never_renumbers() {
        // Line addressability: the lines that survive keep the origin they had
        // in the raw stream, whatever was removed around them.
        let mut block = flat(&[
            "keep me",
            "a.rs:1:1: error: broken",
            "a.rs:2:1: error: broken",
            "a.rs:3:1: error: broken",
            "tail",
        ]);
        group_within(&mut block);
        assert_eq!(block.lines[0].origin, 1);
        assert_eq!(block.lines[1].origin, 2);
        assert_eq!(block.lines[2].origin, 5);
    }

    #[test]
    fn the_same_cause_in_different_files_is_not_grouped() {
        // Thirty files with one fault each is thirty places a reader has to
        // visit. Grouping them keeps one path and takes twenty-nine out of the
        // view, which the fidelity column caught as a lost answer.
        let mut block = flat(&[
            "mod_0.ts(2,9): error TS2322: bad type",
            "mod_1.ts(2,9): error TS2322: bad type",
            "mod_2.ts(2,9): error TS2322: bad type",
        ]);
        group_within(&mut block);
        assert_eq!(block.lines.len(), 3, "every file is still named");
    }

    #[test]
    fn the_same_cause_in_one_file_is_grouped() {
        // The cascade this stage exists for: one fault reported at every use
        // site in the same file.
        let mut block = flat(&[
            "src/lib.rs:3:9: error: mismatched types",
            "src/lib.rs:8:9: error: mismatched types",
            "src/lib.rs:13:9: error: mismatched types",
        ]);
        group_within(&mut block);
        assert_eq!(block.lines.len(), 1);
        assert!(block.lines[0].text.ends_with("[lens: ×3]"), "{}", block.lines[0].text);
    }

    #[test]
    fn a_path_is_a_place_not_a_counter() {
        // The bug the site-coverage column found: two files became one key, and
        // a 96% reduction kept one file of thirty.
        assert_ne!(normalize_sites("mod_0.js"), normalize_sites("mod_1.js"));
        assert_ne!(normalize_sites("/tmp/case/mod_0.js"), normalize_sites("/tmp/case/mod_9.js"));
        // A counter is still a counter.
        assert_eq!(normalize_sites("retry 3 of 9"), normalize_sites("retry 5 of 9"));
        // And a cascade in one file still groups.
        assert_eq!(
            normalize_sites("error[E0308]: mismatched types"),
            normalize_sites("error[E0999]: mismatched types")
        );
    }

    #[test]
    fn findings_in_different_files_keep_their_files() {
        // eslint prints the path as a header and its findings underneath, so
        // the path is the only thing telling two blocks apart.
        let mut doc = doc_of(&["/tmp/mod_0.js", "/tmp/mod_1.js", "/tmp/mod_2.js"]);
        for block in &mut doc.blocks {
            block.class = Class::Error;
        }
        Cause.apply(&mut doc, &Ctx::default());
        assert_eq!(doc.blocks.iter().filter(|b| b.kept()).count(), 3);
    }
}
