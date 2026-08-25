// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Drop the quoted source when there are many findings.
//!
//! A diagnostic quotes the code it is about, under a gutter:
//!
//! ```text
//! F401 [*] `os` imported but unused
//!  --> mod_0.py:1:8
//!   |
//! 1 | import os
//!   |        ^^
//!   |
//! ```
//!
//! That excerpt is the one part of a finding the reader already has. It is in
//! the file, at the line the finding just named. For one finding it earns its
//! space by saving the reader from opening anything; for two hundred it is two
//! hundred copies of code they can already see, and it is most of the output.
//!
//! So it goes only when there are many findings, and only the gutter goes: the
//! message stays, the location stays, and the help that says what to do stays.
//! Nothing here removes a finding.

use super::{Block, Class, Ctx, Doc, Stage};
use crate::adapters::generic::is_gutter;

/// Below this many findings in the view, the excerpt is worth its space.
///
/// A handful of diagnostics is a page a reader works through, and the quoted
/// source is what makes that quick. The waste starts when the findings are more
/// than anyone reads one at a time.
const MIN_FINDINGS: usize = 5;

/// The excerpt stage.
#[derive(Debug, Clone, Copy)]
pub struct Excerpt;

impl Stage for Excerpt {
    fn name(&self) -> &'static str {
        "excerpt"
    }

    fn apply(&self, doc: &mut Doc, _ctx: &Ctx) {
        // Counted over what the view still holds, not over what arrived. A
        // cascade that grouped down to two findings is two findings, and both
        // keep the source that makes them actionable.
        let findings = doc.blocks.iter().filter(|block| is_finding(block)).count();
        if findings < MIN_FINDINGS {
            return;
        }

        for block in &mut doc.blocks {
            if is_finding(block) {
                strip(block);
            }
        }
    }
}

fn is_finding(block: &Block) -> bool {
    block.kept() && matches!(block.class, Class::Error | Class::Warning)
}

/// Remove the gutter from one finding, and say how much went.
fn strip(block: &mut Block) {
    let before = block.lines.len();
    block.lines.retain(|line| !is_gutter(&line.text));
    let removed = before - block.lines.len();

    if removed == 0 {
        return;
    }

    // Announced where it happened, in the compact form the in-block markers
    // use: a hundred findings would otherwise carry a sentence each.
    if let Some(last) = block.lines.last_mut()
        && !last.text.contains("[lens: ")
    {
        last.text.push_str(&format!("  [lens: −{removed}]"));
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::super::{Line, classify};
    use super::*;

    /// A finding with its quoted source, as a compiler prints one.
    fn finding(name: &str, line: usize) -> Block {
        let lines = vec![
            format!("error[E0308]: mismatched types in {name}"),
            format!(" --> {name}:{line}:9"),
            "  |".to_string(),
            format!("{line} |     let x: u64 = None;"),
            "  |                  ^^^^".to_string(),
            "  |".to_string(),
        ];
        let mut block = Block::new(
            lines
                .into_iter()
                .enumerate()
                .map(|(index, text)| Line { text, origin: index + 1 })
                .collect(),
        );
        block.class = Class::Error;
        block
    }

    fn doc_of_findings(count: usize) -> Doc {
        let mut doc = Doc::empty(super::super::Stream::Stdout);
        for n in 0..count {
            doc.blocks.push(finding(&format!("mod_{n}.rs"), n + 1));
        }
        doc
    }

    #[test]
    fn many_findings_lose_their_quoted_source() {
        let mut doc = doc_of_findings(8);
        Excerpt.apply(&mut doc, &Ctx::default());

        for block in &doc.blocks {
            assert_eq!(block.lines.len(), 2, "message and location survive: {:?}", block.text());
            assert!(block.lines[0].text.contains("mismatched types"));
            assert!(block.lines[1].text.contains("mod_"));
        }
    }

    #[test]
    fn a_few_findings_keep_theirs() {
        // Four diagnostics is a page someone reads. The quoted source is what
        // makes that quick, and the space it costs is small.
        let mut doc = doc_of_findings(4);
        let before: Vec<usize> = doc.blocks.iter().map(|b| b.lines.len()).collect();
        Excerpt.apply(&mut doc, &Ctx::default());
        let after: Vec<usize> = doc.blocks.iter().map(|b| b.lines.len()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn what_went_is_announced() {
        let mut doc = doc_of_findings(6);
        Excerpt.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks[0].text().contains("[lens: −4]"), "{}", doc.blocks[0].text());
    }

    #[test]
    fn the_location_survives() {
        // The whole argument for removing the excerpt is that the reader can
        // open the file. Losing the line number would take that away too.
        let mut doc = doc_of_findings(6);
        Excerpt.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks[3].text().contains("mod_3.rs:4:9"), "{}", doc.blocks[3].text());
    }

    #[test]
    fn ordinary_output_is_untouched() {
        let mut doc = doc_of(&["building", "  |  this is not a diagnostic", "done"]);
        classify::Classify.apply(&mut doc, &Ctx::default());
        let before = doc.blocks.iter().map(|b| b.lines.len()).sum::<usize>();
        Excerpt.apply(&mut doc, &Ctx::default());
        assert_eq!(doc.blocks.iter().map(|b| b.lines.len()).sum::<usize>(), before);
    }

    #[test]
    fn nothing_is_renumbered() {
        let mut doc = doc_of_findings(6);
        Excerpt.apply(&mut doc, &Ctx::default());
        assert_eq!(doc.blocks[0].lines[0].origin, 1);
        assert_eq!(doc.blocks[0].lines[1].origin, 2);
    }
}
