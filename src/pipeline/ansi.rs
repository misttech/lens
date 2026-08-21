// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Strip terminal control sequences and resolve carriage-return overwrites.
//!
//! Runs first, because every later stage matches on text. A line that still
//! carries `\x1b[31m` does not match `^error`, and a progress bar that redraws
//! itself with `\r` looks like a thousand distinct lines until the overwrites
//! are applied.
//!
//! This stage rewrites text, which is allowed: removing an escape sequence
//! changes how a line renders, never which line it is.

use super::{Ctx, Doc, Stage};

/// The ANSI stage.
#[derive(Debug, Clone, Copy)]
pub struct Ansi;

impl Stage for Ansi {
    fn name(&self) -> &'static str {
        "ansi"
    }

    fn apply(&self, doc: &mut Doc, _ctx: &Ctx) {
        for block in &mut doc.blocks {
            for line in &mut block.lines {
                if line.text.contains('\x1b') || line.text.contains('\r') {
                    line.text = clean(&line.text);
                }
            }
        }
    }
}

/// Remove escape sequences from `text` and apply its carriage returns.
pub fn clean(text: &str) -> String {
    apply_overwrites(&strip_escapes(text))
}

/// Remove ANSI escape sequences.
///
/// Handles CSI (`\x1b[…final`), OSC (`\x1b]…BEL` or `…ST`), and the two-byte
/// escapes. An unterminated sequence at end of input is dropped rather than
/// emitted, since a half-written escape is not content either.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\x1b' {
            // Other C0 controls are not content, but tab is, and so is the
            // carriage return that the overwrite pass needs to see.
            if c == '\t' || c == '\r' || !c.is_control() {
                out.push(c);
            }
            continue;
        }

        match chars.next() {
            // CSI: parameters and intermediates, then a final byte in @..~
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        break;
                    }
                }
            }
            // OSC: runs to BEL or to ESC \
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\x07' {
                        break;
                    }
                    if next == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Anything else is a two-byte escape; the second byte is consumed.
            Some(_) | None => {}
        }
    }

    out
}

/// Apply carriage returns, keeping only the final state of an overwritten line.
///
/// A `\r` moves the cursor to column zero; what follows overwrites from there.
/// Shorter replacements leave the tail of the previous content visible, which is
/// what a terminal shows and therefore what the reader would have seen.
fn apply_overwrites(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }

    let mut canvas: Vec<char> = Vec::new();
    let mut cursor = 0usize;

    for c in text.chars() {
        if c == '\r' {
            cursor = 0;
            continue;
        }
        if cursor < canvas.len() {
            canvas[cursor] = c;
        } else {
            canvas.push(c);
        }
        cursor += 1;
    }

    canvas.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::*;

    #[test]
    fn color_codes_are_removed() {
        assert_eq!(clean("\x1b[31merror\x1b[0m: boom"), "error: boom");
        assert_eq!(clean("\x1b[1;38;5;196mred\x1b[m"), "red");
    }

    #[test]
    fn stripping_makes_later_matching_work() {
        // The reason this stage runs first: a colored error does not match
        // `error` until the escape is gone.
        let colored = "\x1b[31merror\x1b[0m[E0308]";
        assert!(!colored.starts_with("error"));
        assert!(clean(colored).starts_with("error"));
    }

    #[test]
    fn cursor_movement_and_erase_sequences_are_removed() {
        assert_eq!(clean("\x1b[2K\x1b[1Gprogress"), "progress");
        assert_eq!(clean("done\x1b[K"), "done");
    }

    #[test]
    fn operating_system_commands_are_removed() {
        // Terminal title sets, hyperlinks. Both terminator forms.
        assert_eq!(clean("\x1b]0;my title\x07text"), "text");
        assert_eq!(clean("\x1b]8;;https://example.com\x1b\\link"), "link");
    }

    #[test]
    fn an_unterminated_escape_does_not_leak() {
        assert_eq!(clean("text\x1b[31"), "text");
        assert_eq!(clean("text\x1b"), "text");
    }

    #[test]
    fn tabs_survive_and_other_controls_do_not() {
        // A tab is layout the reader sees; a bell is not content.
        assert_eq!(clean("a\tb"), "a\tb");
        assert_eq!(clean("a\x07b"), "ab");
        assert_eq!(clean("a\x00b"), "ab");
    }

    #[test]
    fn carriage_returns_keep_only_the_final_state() {
        // A progress bar redrawing in place: only what the reader ended up
        // seeing survives.
        assert_eq!(clean("10%\r50%\r100%"), "100%");
        assert_eq!(clean("downloading\rdone"), "doneloading");
    }

    #[test]
    fn an_overwrite_leaves_the_uncovered_tail() {
        // What a real terminal shows. Surprising, but inventing a cleaner answer
        // would mean showing the reader something that was never on screen.
        assert_eq!(clean("abcdef\rxy"), "xycdef");
    }

    #[test]
    fn text_without_escapes_is_untouched() {
        let plain = "error: could not compile `lens`";
        assert_eq!(clean(plain), plain);
    }

    #[test]
    fn the_stage_leaves_line_numbers_alone() {
        let mut doc = doc_of(&["\x1b[31mred\x1b[0m", "plain"]);
        Ansi.apply(&mut doc, &Ctx::default());
        assert_eq!(doc.blocks[0].lines[0].text, "red");
        assert_eq!(doc.blocks[0].lines[0].origin, 1);
        assert_eq!(doc.blocks[1].lines[0].origin, 2);
    }

    #[test]
    fn the_stage_drops_nothing() {
        // Stripping is not filtering: every line is still here afterwards.
        let mut doc = doc_of(&["\x1b[31m\x1b[0m", "plain"]);
        Ansi.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks.iter().all(|b| b.kept()));
    }
}
