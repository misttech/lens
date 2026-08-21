// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Keep what makes a failure actionable.
//!
//! An error message on its own says something went wrong. The blocks around it
//! say what was being attempted, which file, which test, what the arguments
//! were — and that is the difference between a reader who can act and a reader
//! who has to ask for more.
//!
//! So context is force-kept rather than merely kept: budget pressure may drop an
//! error's neighbours only after it has dropped everything else, and this stage
//! is what says so.

use super::{Class, Ctx, Doc, Stage};

/// The context stage.
#[derive(Debug, Clone, Copy)]
pub struct Context;

impl Stage for Context {
    fn name(&self) -> &'static str {
        "context"
    }

    fn apply(&self, doc: &mut Doc, ctx: &Ctx) {
        let failures: Vec<usize> = doc
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| matches!(block.class, Class::Error | Class::Failure))
            .map(|(index, _)| index)
            .collect();

        for index in failures {
            let first = index.saturating_sub(ctx.context_blocks);
            let last = (index + ctx.context_blocks).min(doc.blocks.len().saturating_sub(1));

            for neighbour in first..=last {
                // Progress churn next to an error is still churn. Rescuing it
                // would spend the budget that the error's real context needs.
                if doc.blocks[neighbour].class == Class::Progress {
                    continue;
                }
                doc.blocks[neighbour].force();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::super::{Keep, classify};
    use super::*;

    /// Classify then apply context, which is the order the pipeline uses.
    fn run(lines: &[&str], context_blocks: usize) -> Doc {
        let mut doc = doc_of(lines);
        let ctx = Ctx { exit_code: 0, context_blocks };
        classify::Classify.apply(&mut doc, &ctx);
        Context.apply(&mut doc, &ctx);
        doc
    }

    #[test]
    fn blocks_around_a_failure_are_forced() {
        let doc = run(&["far", "before", "error: boom", "after", "far"], 1);
        let forced: Vec<bool> = doc.blocks.iter().map(|b| b.keep == Keep::Force).collect();
        assert_eq!(forced, vec![false, true, true, true, false]);
    }

    #[test]
    fn context_reaches_as_far_as_configured() {
        let doc = run(&["a", "b", "c", "error: boom", "e", "f", "g"], 3);
        assert!(doc.blocks.iter().all(|b| b.keep == Keep::Force));
    }

    #[test]
    fn zero_context_forces_only_the_failure() {
        let doc = run(&["before", "error: boom", "after"], 0);
        let forced: Vec<bool> = doc.blocks.iter().map(|b| b.keep == Keep::Force).collect();
        assert_eq!(forced, vec![false, true, false]);
    }

    #[test]
    fn context_at_the_edges_does_not_panic() {
        let doc = run(&["error: first thing"], 3);
        assert_eq!(doc.blocks[0].keep, Keep::Force);

        let doc = run(&["ok", "error: last thing"], 3);
        assert!(doc.blocks.iter().all(|b| b.keep == Keep::Force));
    }

    #[test]
    fn context_rescues_a_block_an_earlier_stage_dropped() {
        // A duplicate line next to an error is the error's context, and the
        // reader needs it more than dedupe needed to remove it.
        let mut doc = doc_of(&["running check", "error: boom"]);
        doc.blocks[0].drop_with("dedupe");

        let ctx = Ctx { exit_code: 0, context_blocks: 1 };
        classify::Classify.apply(&mut doc, &ctx);
        Context.apply(&mut doc, &ctx);

        assert!(doc.blocks[0].kept());
        assert!(doc.blocks[0].elided.is_none(), "nothing is reported as removed");
    }

    #[test]
    fn progress_next_to_a_failure_stays_dropped() {
        // Churn does not become worth reading by being adjacent to an error,
        // and the budget it would take belongs to the real context.
        let mut doc = doc_of(&["   Compiling foo v1.0", "error: boom"]);
        let ctx = Ctx { exit_code: 0, context_blocks: 1 };
        super::super::progress::Progress.apply(&mut doc, &ctx);
        classify::Classify.apply(&mut doc, &ctx);
        Context.apply(&mut doc, &ctx);

        assert!(!doc.blocks[0].kept(), "progress is not rescued");
        assert!(doc.blocks[1].kept());
    }

    #[test]
    fn a_document_without_failures_is_untouched() {
        let doc = run(&["one", "two", "three"], 3);
        assert!(doc.blocks.iter().all(|b| b.keep != Keep::Force));
    }

    #[test]
    fn overlapping_windows_do_not_conflict() {
        let doc = run(&["a", "error: one", "b", "error: two", "c"], 1);
        assert!(doc.blocks.iter().all(|b| b.keep == Keep::Force));
    }
}
