// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! The fallback adapter: line-based blocking that works on anything.
//!
//! Two separators, both of which mean "a new thought starts here" in almost
//! every command's output:
//!
//! * a blank line;
//! * a return to the left margin after indented lines, which is how compilers,
//!   test runners and tracebacks all mark the end of a stanza.
//!
//! Getting this wrong is cheap in one direction and expensive in the other. Too
//! many blocks costs a little precision in what gets dropped together; too few
//! means an error and a hundred lines of noise share a fate.

use crate::pipeline::{Block, Doc, Line, Stream};

/// Parse text into blocks.
pub fn parse(text: &str, stream: Stream) -> Doc {
    let mut doc = Doc::empty(stream);
    let mut current: Vec<Line> = Vec::new();
    // Whether the block being gathered has an indented continuation in it.
    // Carried rather than rediscovered: asking the question by scanning the
    // block makes parsing quadratic in the length of a block, and a command
    // that prints ten thousand unindented lines is one block.
    let mut has_continuation = false;

    for (index, raw) in text.lines().enumerate() {
        let line = Line { text: raw.to_string(), origin: index + 1 };

        if raw.trim().is_empty() {
            // A blank line ends the block and is not content of its own.
            flush(&mut doc, &mut current);
            has_continuation = false;
            continue;
        }

        let indented = is_indented(raw);

        // A line at the left margin after an indented run starts a new stanza.
        if has_continuation && !indented && !is_gutter(raw) {
            flush(&mut doc, &mut current);
            has_continuation = false;
        }

        // The first line of a block is its header; an indented line after it is
        // the continuation that makes the next return to the margin a boundary.
        if indented && !current.is_empty() {
            has_continuation = true;
        }
        current.push(line);
    }

    flush(&mut doc, &mut current);
    doc
}

fn is_indented(text: &str) -> bool {
    text.starts_with(' ') || text.starts_with('\t')
}

/// Is this an unindented continuation rather than a new stanza?
///
/// Compilers print source excerpts in a numbered gutter — `4  |     let x = 1;`
/// — which starts at the left margin without starting anything. Treating those
/// as new blocks would split a diagnostic away from the code it is about, which
/// is precisely the context a reader needs.
pub(crate) fn is_gutter(text: &str) -> bool {
    let rest = text.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.trim_start();
    rest.starts_with('|')
        || rest.starts_with('=')
        || rest.starts_with('^')
        || rest.starts_with("...")
}

/// Move the gathered lines into the document as one block.
fn flush(doc: &mut Doc, current: &mut Vec<Line>) {
    if !current.is_empty() {
        doc.blocks.push(Block::new(std::mem::take(current)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(text: &str) -> Vec<String> {
        parse(text, Stream::Stdout).blocks.iter().map(|b| b.text()).collect()
    }

    #[test]
    fn blank_lines_separate_blocks() {
        assert_eq!(blocks("one\n\ntwo\n\nthree"), vec!["one", "two", "three"]);
    }

    #[test]
    fn consecutive_blank_lines_do_not_make_empty_blocks() {
        assert_eq!(blocks("one\n\n\n\ntwo"), vec!["one", "two"]);
    }

    #[test]
    fn an_indented_stanza_stays_with_its_header() {
        // The shape every compiler diagnostic has. Splitting the message from
        // its location would let one be dropped without the other.
        let text = "error[E0308]: mismatched types\n  --> src/main.rs:4:9\n   |\n4  |     let x: u8 = \"s\";\n";
        assert_eq!(blocks(text).len(), 1);
        assert!(blocks(text)[0].contains("E0308"));
    }

    #[test]
    fn a_numbered_gutter_line_is_not_a_new_block() {
        // The shape rustc, gcc and clang all print. Splitting here would put the
        // diagnostic in one block and the source it points at in another.
        let text = "error[E0308]: mismatched types\n  --> src/main.rs:4:9\n   |\n4  |     let x: u8 = \"s\";\n   |                 ^^^ expected `u8`\n";
        let parsed = blocks(text);
        assert_eq!(parsed.len(), 1, "{parsed:?}");
    }

    #[test]
    fn a_return_to_the_margin_starts_a_new_block() {
        let text = "error: first\n  detail\nerror: second\n  detail";
        let parsed = blocks(text);
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        assert!(parsed[0].starts_with("error: first"));
        assert!(parsed[1].starts_with("error: second"));
    }

    #[test]
    fn a_flat_list_is_one_block() {
        // No indentation anywhere, so there is no stanza structure to find.
        assert_eq!(blocks("a.rs\nb.rs\nc.rs").len(), 1);
    }

    #[test]
    fn line_numbers_are_one_based_and_count_blank_lines() {
        // The property that keeps `file:line` references resolvable: origin is
        // the line's position in the raw stream, not in the filtered view.
        let doc = parse("one\n\nthree\n", Stream::Stdout);
        assert_eq!(doc.blocks[0].lines[0].origin, 1);
        assert_eq!(doc.blocks[1].lines[0].origin, 3);
    }

    #[test]
    fn origins_are_strictly_increasing() {
        let doc = parse("a\n  b\nc\n\nd\n  e\n", Stream::Stdout);
        let origins: Vec<usize> =
            doc.blocks.iter().flat_map(|b| &b.lines).map(|l| l.origin).collect();
        assert!(origins.windows(2).all(|w| w[0] < w[1]), "{origins:?}");
    }

    #[test]
    fn empty_input_is_an_empty_document() {
        assert!(blocks("").is_empty());
        assert!(blocks("\n\n\n").is_empty());
    }

    #[test]
    fn a_missing_final_newline_does_not_lose_the_last_line() {
        assert_eq!(blocks("one\ntwo"), vec!["one\ntwo"]);
    }

    #[test]
    fn every_line_survives_parsing() {
        // Blocking is not filtering. Blank lines are separators rather than
        // content, so they are the only thing parsing removes.
        let text = "a\n  b\nc\n\nd\n";
        let doc = parse(text, Stream::Stdout);
        let parsed: usize = doc.blocks.iter().map(|b| b.lines.len()).sum();
        let non_blank = text.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(parsed, non_blank);
    }

    #[test]
    fn tabs_count_as_indentation() {
        let text = "header\n\tdetail\nnext";
        assert_eq!(blocks(text).len(), 2);
    }

    #[test]
    fn the_stream_is_recorded() {
        assert_eq!(parse("x", Stream::Stderr).source, Stream::Stderr);
    }
}
