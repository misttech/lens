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
use super::{Block, Class, Ctx, Doc, Stage};

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

/// The cause a block reports, or `None` when it does not report one.
///
/// Ordinary output is never grouped: two identical lines of program output are
/// [`super::dedupe`]'s business, and collapsing them here would elide content
/// nobody classified as a diagnostic.
fn key_for(block: &Block) -> Option<String> {
    if !matches!(block.class, Class::Error | Class::Failure | Class::Warning) {
        return None;
    }
    // Digits carry the line number, the column, and the counter in the
    // identifier — everything that differs between two reports of one cause.
    Some(normalize(&block.lines.first()?.text))
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
}
