// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Fit the view into a token budget, without ever deleting a failure.
//!
//! Rank has ordered the blocks. This stage walks that order from cheapest to
//! dearest and marks blocks [`Keep::Drop`] until the estimate fits, or until
//! only the irreducible core remains: every `Error`/`Failure` and the context
//! already forced around them.
//!
//! A budget that cannot contain the core is not resolved by deleting an error.
//! The overrun is reported as [`Doc::budget_exceeded`] and the core is emitted
//! anyway — an unhelpful view is recoverable, a silent one is not.
//!
//! Not a [`Stage`]: a token budget is for the invocation, so this sees every
//! stream at once. Applying it per descriptor would give each pipe a full
//! budget and double what the caller asked for.

use super::{Class, Ctx, Doc, Keep};

use crate::tokens::{Heuristic, TokenEstimator};

/// Fit `docs` into `ctx.budget`, dropping by ascending score.
///
/// Does nothing when no budget was asked for. Ranking still ran; nothing is
/// dropped for want of a number nobody chose.
pub fn apply(docs: &mut [&mut Doc], ctx: &Ctx) {
    let Some(budget) = ctx.budget else {
        return;
    };

    if tokens_kept(docs) <= budget {
        return;
    }

    let mut order: Vec<(usize, usize, u32)> = Vec::new();
    for (doc_i, doc) in docs.iter().enumerate() {
        for (block_i, block) in doc.blocks.iter().enumerate() {
            if droppable(block) {
                // f32 is not Ord. The score lives in [0, 1]; millipoints keep
                // the ranking and give a total order for the sort.
                let millipoints = (block.score * 1000.0).round() as u32;
                order.push((doc_i, block_i, millipoints));
            }
        }
    }
    order.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));

    for (doc_i, block_i, _) in order {
        if tokens_kept(docs) <= budget {
            break;
        }
        docs[doc_i].blocks[block_i].drop_with("budget");
    }

    if tokens_kept(docs) > budget {
        for doc in docs.iter_mut() {
            doc.budget_exceeded = true;
        }
    }
}

fn droppable(block: &super::Block) -> bool {
    // The core is never a candidate. Force covers context; the class check
    // covers an Error that somehow arrived unforced — budget does not become
    // the stage that hides a failure, even if context was skipped.
    block.keep == Keep::Normal && !matches!(block.class, Class::Error | Class::Failure)
}

fn tokens_in(doc: &Doc) -> usize {
    let estimator = Heuristic;
    doc.blocks.iter().filter(|b| b.kept()).map(|b| estimator.estimate(&b.text())).sum()
}

fn tokens_kept(docs: &[&mut Doc]) -> usize {
    docs.iter().map(|doc| tokens_in(doc)).sum()
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::super::{Class, Keep, Stage, classify, context, rank};
    use super::*;

    fn prepared(lines: &[&str], exit_code: i32) -> Doc {
        let mut doc = doc_of(lines);
        let ctx = Ctx { exit_code, ..Ctx::default() };
        classify::Classify.apply(&mut doc, &ctx);
        context::Context.apply(&mut doc, &ctx);
        rank::Rank.apply(&mut doc, &ctx);
        doc
    }

    #[test]
    fn no_budget_drops_nothing() {
        let mut doc = prepared(&["one", "two", "three"], 0);
        apply(&mut [&mut doc], &Ctx::default());
        assert!(doc.blocks.iter().all(|b| b.kept()));
        assert!(!doc.budget_exceeded);
    }

    #[test]
    fn a_generous_budget_drops_nothing() {
        let mut doc = prepared(&["one", "two", "three"], 0);
        apply(&mut [&mut doc], &Ctx { budget: Some(10_000), ..Ctx::default() });
        assert!(doc.blocks.iter().all(|b| b.kept()));
    }

    #[test]
    fn a_tight_budget_drops_the_cheapest_first() {
        // Enough ordinary lines that context around the error cannot cover them
        // all, so something remains droppable.
        let mut lines: Vec<String> =
            (0..20).map(|i| format!("ordinary output line {i} with some text")).collect();
        lines.push("error: boom".into());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut doc = prepared(&refs, 1);
        let before = tokens_kept(&[&mut doc]);
        apply(&mut [&mut doc], &Ctx { exit_code: 1, budget: Some(before / 2), ..Ctx::default() });

        let error = doc.blocks.iter().find(|b| b.class == Class::Error).unwrap();
        assert!(error.kept(), "the failure survives");
        assert!(doc.blocks.iter().any(|b| !b.kept()), "something cheaper went");
        assert_eq!(
            doc.blocks.iter().find(|b| !b.kept()).map(|b| b.elided.as_ref().unwrap().reason),
            Some("budget")
        );
    }

    #[test]
    fn forced_blocks_always_survive_budget_pressure() {
        // The property the whole tool rests on under a budget: a failure, and
        // the context that makes it actionable, cannot be deleted to make a
        // number fit.
        let mut doc = prepared(&["setup", "error: boom", "teardown", "noise at the end"], 1);
        apply(&mut [&mut doc], &Ctx { exit_code: 1, budget: Some(1), ..Ctx::default() });

        for block in &doc.blocks {
            if block.keep == Keep::Force || matches!(block.class, Class::Error | Class::Failure) {
                assert!(block.kept(), "forced content was dropped: {}", block.text());
            }
        }
        assert!(doc.budget_exceeded, "a budget of 1 token cannot hold the core");
    }

    #[test]
    fn an_error_is_never_dropped_even_if_unforced() {
        let mut doc = doc_of(&["ordinary", "error: boom"]);
        classify::Classify.apply(&mut doc, &Ctx::default());
        rank::Rank.apply(&mut doc, &Ctx::default());
        assert_eq!(doc.blocks[1].keep, Keep::Normal, "context did not run");

        apply(&mut [&mut doc], &Ctx { budget: Some(1), ..Ctx::default() });
        assert!(doc.blocks[1].kept(), "the error stands without being forced");
        assert!(!doc.blocks[0].kept());
    }

    #[test]
    fn both_streams_share_one_budget() {
        let mut out = prepared(&["a generous amount of ordinary stdout text"], 0);
        let mut err = prepared(&["more ordinary stderr text as well"], 0);
        let together = tokens_kept(&[&mut out, &mut err]);
        apply(&mut [&mut out, &mut err], &Ctx { budget: Some(together / 2), ..Ctx::default() });
        let leftover = tokens_kept(&[&mut out, &mut err]);
        assert!(leftover <= together / 2 || out.budget_exceeded || err.budget_exceeded);
        assert!(
            out.blocks.iter().any(|b| !b.kept()) || err.blocks.iter().any(|b| !b.kept()),
            "the shared budget has to come from somewhere"
        );
    }

    #[test]
    fn dropping_stops_at_the_core() {
        let mut doc = prepared(&["error: boom"], 1);
        apply(&mut [&mut doc], &Ctx { exit_code: 1, budget: Some(0), ..Ctx::default() });
        assert!(doc.blocks[0].kept());
        assert!(doc.budget_exceeded);
    }
}
