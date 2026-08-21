// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The document model and the stage runner.
//!
//! Stages operate on a structured document rather than on a string. That is what
//! makes them composable, and it is what makes an elision marker possible: a
//! stage that wants content gone marks the block, and the renderer — the one
//! place that removes anything — knows how much was removed and why.
//!
//! Three rules hold for every stage:
//!
//! * A stage may mark, merge, or annotate blocks. It may not delete them.
//! * A stage may not mutate [`Line::origin`]. Line numbers in the raw stream are
//!   what make a `file:line` reference in the output still resolve.
//! * Dropping means setting [`Keep::Drop`] and recording an [`Elision`]. What was
//!   removed is always countable, and therefore always announceable.

use crate::static_assert_size_and_align;

pub mod ansi;
pub mod classify;
pub mod context;
pub mod dedupe;
pub mod progress;

/// Which stream a document came from.
///
/// Kept separate all the way through: a line's stream is a signal, and merging
/// the two would discard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// The child's stdout.
    Stdout,
    /// The child's stderr.
    Stderr,
}

/// One line of output, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The line's text, without its terminator.
    pub text: String,
    /// 1-based index in the raw stream. Never mutated, by any stage, ever.
    pub origin: usize,
}

// One `Line` per input line, and a large fixture is hundreds of thousands of
// them. A field added here is a throughput regression that the benchmark would
// report only as a mystery, so the size is pinned at the declaration.
static_assert_size_and_align!(Line, 32, 8);

/// What a block is, as far as filtering is concerned.
///
/// Severity, not structure. Structure arrives with the adapters that can
/// actually recognize it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Class {
    /// Something failed and the reader needs to see it.
    Error,
    /// A reported failure — a test, a check — as distinct from an error message.
    Failure,
    /// Worth knowing, not worth acting on immediately.
    Warning,
    /// Ordinary output.
    #[default]
    Info,
    /// A progress indicator: a spinner, a percentage, a download counter.
    Progress,
    /// Content with no information in it at all.
    Noise,
}

static_assert_size_and_align!(Class, 1, 1);

/// Whether a block survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keep {
    /// Survives regardless of budget pressure.
    Force,
    /// Survives unless something drops it.
    #[default]
    Normal,
    /// Removed by the renderer, with its elision announced.
    Drop,
}

/// A record of content outside the current view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elision {
    /// How many lines are not being shown.
    pub lines_removed: usize,
    /// Which stage removed them, for the marker and the debug report.
    pub reason: &'static str,
}

/// A run of lines treated as one unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The lines, in order.
    pub lines: Vec<Line>,
    /// Assigned by the classify stage.
    pub class: Class,
    /// Whether the renderer will emit it.
    pub keep: Keep,
    /// Set when `keep` is [`Keep::Drop`].
    pub elided: Option<Elision>,
}

impl Block {
    /// A block of ordinary output.
    pub fn new(lines: Vec<Line>) -> Self {
        Block { lines, class: Class::default(), keep: Keep::default(), elided: None }
    }

    /// Mark this block as outside the view, recording why.
    ///
    /// A forced block is never dropped: whatever wanted it gone loses to
    /// whatever insisted it stay, and the caller is told nothing was removed.
    pub fn drop_with(&mut self, reason: &'static str) -> bool {
        if self.keep == Keep::Force {
            return false;
        }
        self.keep = Keep::Drop;
        self.elided = Some(Elision { lines_removed: self.lines.len(), reason });
        true
    }

    /// Keep this block regardless of what any later stage wants.
    pub fn force(&mut self) {
        self.keep = Keep::Force;
        self.elided = None;
    }

    /// Is this block part of the view?
    pub fn kept(&self) -> bool {
        self.keep != Keep::Drop
    }

    /// The block's text, lines joined with newlines.
    pub fn text(&self) -> String {
        self.lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n")
    }
}

/// One stream, parsed into blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    /// The blocks, in the order they appeared.
    pub blocks: Vec<Block>,
    /// Which stream this came from.
    pub source: Stream,
}

impl Doc {
    /// An empty document for `source`.
    pub fn empty(source: Stream) -> Self {
        Doc { blocks: Vec::new(), source }
    }

    /// Total lines across every block, dropped ones included.
    pub fn line_count(&self) -> usize {
        self.blocks.iter().map(|block| block.lines.len()).sum()
    }

    /// Lines that survive into the view.
    pub fn kept_line_count(&self) -> usize {
        self.blocks.iter().filter(|b| b.kept()).map(|b| b.lines.len()).sum()
    }

    /// Lines removed, summed from what the stages recorded.
    ///
    /// Test-only: the production path derives the same figure by subtracting
    /// kept lines from the total, and two ways of counting the same thing is
    /// one way for them to disagree. Here it checks that the elisions stages
    /// recorded add up to what actually left the view.
    #[cfg(test)]
    pub fn elided_line_count(&self) -> usize {
        self.blocks.iter().filter_map(|b| b.elided.as_ref()).map(|e| e.lines_removed).sum()
    }

    /// Does this document have anything classified as a failure?
    pub fn has_failure(&self) -> bool {
        self.blocks.iter().any(|b| matches!(b.class, Class::Error | Class::Failure))
    }
}

/// What a stage needs to know about the run it is filtering.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    /// The code the command exited with. A non-zero code changes what may be
    /// dropped: output that hides a failure is the worst thing this tool can
    /// produce.
    pub exit_code: i32,
    /// How many blocks of context to keep around a failure.
    pub context_blocks: usize,
}

impl Default for Ctx {
    fn default() -> Self {
        Ctx { exit_code: 0, context_blocks: 3 }
    }
}

/// One filtering step.
///
/// Stages do not return errors. A stage that cannot do its job leaves the
/// document alone, because the alternative — failing the run — would turn a
/// filtering problem into the user's problem.
pub trait Stage {
    /// The stage's name, as it appears in the debug report and the log.
    fn name(&self) -> &'static str;

    /// Mark, merge or annotate. Never delete.
    fn apply(&self, doc: &mut Doc, ctx: &Ctx);
}

/// Run `stages` over `doc`, in order.
pub fn run(doc: &mut Doc, stages: &[&dyn Stage], ctx: &Ctx) {
    for stage in stages {
        stage.apply(doc, ctx);
    }
}

/// The default stage list.
pub fn default_stages() -> Vec<&'static dyn Stage> {
    vec![&ansi::Ansi, &progress::Progress, &dedupe::Dedupe, &classify::Classify, &context::Context]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document of single-line blocks, numbered from 1.
    pub(crate) fn doc_of(lines: &[&str]) -> Doc {
        let blocks = lines
            .iter()
            .enumerate()
            .map(|(i, text)| Block::new(vec![Line { text: (*text).to_string(), origin: i + 1 }]))
            .collect();
        Doc { blocks, source: Stream::Stdout }
    }

    #[test]
    fn filtering_is_linear_in_the_input() {
        // Parsing was quadratic once: it asked "does this block have an indented
        // line?" by scanning the block, for every line, and a command that
        // prints ten thousand unindented lines is one block. 40k lines took
        // 451ms. This is the guard that keeps it from coming back.
        let time_for = |n: usize| {
            let text: String = (1..=n).map(|i| format!("line number {i}\n")).collect();
            let start = std::time::Instant::now();
            let mut doc = crate::adapters::parse(text.as_bytes(), Stream::Stdout);
            run(&mut doc, &default_stages(), &Ctx::default());
            let _ = crate::render::render(&doc, crate::render::Level::Detail, Some("h"));
            start.elapsed().as_secs_f64()
        };

        // Warm the allocator so the first measurement is not the slowest.
        let _ = time_for(2_000);

        let small = time_for(10_000).max(1e-6);
        let large = time_for(40_000);
        let growth = large / small;

        // Four times the input should cost about four times as much. The bound
        // is generous because this runs on shared CI hardware; the failure it
        // catches is quadratic growth, which showed up here as 14x.
        assert!(growth < 10.0, "4x the input cost {growth:.1}x the time");
    }

    #[test]
    fn dropping_a_block_records_what_was_removed() {
        let mut block = Block::new(vec![
            Line { text: "a".into(), origin: 1 },
            Line { text: "b".into(), origin: 2 },
        ]);
        assert!(block.drop_with("progress"));
        assert!(!block.kept());
        let elision = block.elided.expect("an elision is recorded");
        assert_eq!(elision.lines_removed, 2);
        assert_eq!(elision.reason, "progress");
    }

    #[test]
    fn a_forced_block_cannot_be_dropped() {
        // The rule that makes context worth having: whatever insisted a block
        // stay outranks whatever later wants it gone.
        let mut block = Block::new(vec![Line { text: "panic!".into(), origin: 9 }]);
        block.force();
        assert!(!block.drop_with("budget"), "the drop is refused");
        assert!(block.kept());
        assert!(block.elided.is_none(), "and nothing is reported as removed");
    }

    #[test]
    fn forcing_a_dropped_block_clears_its_elision() {
        let mut block = Block::new(vec![Line { text: "x".into(), origin: 1 }]);
        block.drop_with("progress");
        block.force();
        assert!(block.kept());
        assert!(block.elided.is_none(), "a block in the view has nothing elided");
    }

    #[test]
    fn counts_split_between_kept_and_elided() {
        let mut doc = doc_of(&["one", "two", "three"]);
        doc.blocks[1].drop_with("progress");
        assert_eq!(doc.line_count(), 3);
        assert_eq!(doc.kept_line_count(), 2);
        assert_eq!(doc.elided_line_count(), 1);
    }

    #[test]
    fn stages_run_in_order() {
        struct Marker(&'static str);
        impl Stage for Marker {
            fn name(&self) -> &'static str {
                self.0
            }
            fn apply(&self, doc: &mut Doc, _ctx: &Ctx) {
                doc.blocks[0].lines[0].text.push_str(self.0);
            }
        }

        let mut doc = doc_of(&[""]);
        let (first, second) = (Marker("1"), Marker("2"));
        run(&mut doc, &[&first, &second], &Ctx::default());
        assert_eq!(doc.blocks[0].lines[0].text, "12");
    }

    #[test]
    fn no_stage_mutates_a_line_origin() {
        // The property that keeps `file:line` references resolving. Asserted
        // here across the whole default stage list rather than per stage, so a
        // new stage inherits the check.
        let mut doc = doc_of(&[
            "error: something failed",
            "  --> src/main.rs:12:5",
            "Compiling foo v0.1.0",
            " 45% downloading",
            "warning: unused",
        ]);
        let before: Vec<usize> =
            doc.blocks.iter().flat_map(|b| &b.lines).map(|l| l.origin).collect();

        let stages = default_stages();
        run(&mut doc, &stages, &Ctx::default());

        let after: Vec<usize> =
            doc.blocks.iter().flat_map(|b| &b.lines).map(|l| l.origin).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn no_stage_removes_a_block() {
        // Removal happens once, in the renderer. A stage that deletes would make
        // the elision uncountable, and an uncountable elision cannot be
        // announced.
        let mut doc = doc_of(&["Compiling foo", " 10%", " 20%", "error: boom", "done"]);
        let before = doc.blocks.len();

        let stages = default_stages();
        run(&mut doc, &stages, &Ctx::default());

        assert_eq!(doc.blocks.len(), before);
        assert!(doc.blocks.iter().any(|b| !b.kept()), "something was marked, not deleted");
    }

    #[test]
    fn no_line_is_lost_without_being_counted() {
        // The accounting identity behind every marker: what the reader sees plus
        // what the stages recorded as removed equals what the command produced.
        // A stage that drops a line without recording it breaks this, and a
        // broken elision count is content that disappears unannounced.
        let inputs: [&[&str]; 5] = [
            &["warning: a", "warning: a"],
            &["warning: a", "warning: a", "warning: a"],
            &["   Compiling foo v1.0", "error: boom", "   Compiling bar v1.0"],
            &["a", "b", "a", "b", "a", "b"],
            &["one", "", "two"],
        ];

        for lines in inputs {
            for exit_code in [0, 1] {
                let mut doc = doc_of(lines);
                let before = doc.line_count();
                run(&mut doc, &default_stages(), &Ctx { exit_code, ..Ctx::default() });

                assert_eq!(
                    doc.kept_line_count() + doc.elided_line_count(),
                    before,
                    "{lines:?} at exit {exit_code}: {} kept + {} elided != {before}",
                    doc.kept_line_count(),
                    doc.elided_line_count()
                );
            }
        }
    }

    #[test]
    fn every_dropped_block_carries_an_elision() {
        let mut doc = doc_of(&["Compiling foo", " 42% done", "ok"]);
        let stages = default_stages();
        run(&mut doc, &stages, &Ctx::default());

        for block in doc.blocks.iter().filter(|b| !b.kept()) {
            let elision = block.elided.as_ref().expect("a dropped block says what went");
            assert_eq!(elision.lines_removed, block.lines.len());
            assert!(!elision.reason.is_empty());
        }
    }
}
