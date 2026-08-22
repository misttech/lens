// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Structured parsing of `git status`, `diff`, `log` and `show`.
//!
//! The contract that makes this adapter worth having is line addressability:
//! a hunk header is stored verbatim, and elision drops the whole hunk. Rewriting
//! `@@` to close a gap would make every later `file:line` in the view lie.

use crate::pipeline::{Block, Doc, Kind, Line, Stream};

/// Why git output could not be parsed as git output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// What went wrong, for the warn that accompanies a generic fallback.
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Parse git output into a structured document.
///
/// # Errors
///
/// Returns [`ParseError`] when the stream is not git-shaped, or when a hunk
/// header is present but not a unified-diff `@@` line. Callers fall back to
/// the generic adapter rather than failing the run.
pub fn parse(text: &str, stream: Stream) -> Result<Doc, ParseError> {
    if text.is_empty() {
        return Ok(Doc::empty(stream));
    }
    if looks_like_diff_or_log(text) {
        return parse_diff_and_log(text, stream);
    }
    if looks_like_status(text) {
        return parse_status(text, stream);
    }
    Err(ParseError { message: "not git status, diff, log or show output".into() })
}

fn looks_like_diff_or_log(text: &str) -> bool {
    text.lines().any(|line| {
        line.starts_with("diff --git ")
            || is_hunk_header(line)
            || is_commit_line(line)
            || is_oneline_log(line) && text.lines().filter(|l| !l.is_empty()).all(is_oneline_log)
    })
}

fn looks_like_status(text: &str) -> bool {
    text.lines().any(|line| {
        line.starts_with("On branch ")
            || line.starts_with("HEAD detached ")
            || line == "Not currently on any branch."
            || line.starts_with("Changes to be committed:")
            || line.starts_with("Changes not staged for commit:")
            || line.starts_with("Untracked files:")
            || line.starts_with("Unmerged paths:")
            || is_porcelain(line)
    })
}

fn is_commit_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("commit ") else {
        return false;
    };
    let hash = rest.split_whitespace().next().unwrap_or("");
    hash.len() >= 7 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_oneline_log(line: &str) -> bool {
    let Some((hash, rest)) = line.split_once(' ') else {
        return false;
    };
    !rest.is_empty()
        && hash.len() >= 7
        && hash.len() <= 40
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_porcelain(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.len() < 4 || bytes[2] != b' ' {
        return false;
    }
    is_porcelain_xy(bytes[0]) && is_porcelain_xy(bytes[1])
}

fn is_porcelain_xy(b: u8) -> bool {
    matches!(b, b' ' | b'M' | b'A' | b'D' | b'R' | b'C' | b'U' | b'?' | b'!')
}

/// A unified-diff hunk header, whose text must never be rewritten.
fn is_hunk_header(line: &str) -> bool {
    let Some(after) = line.strip_prefix("@@ ") else {
        return false;
    };
    let spec = match after.split_once(" @@") {
        Some((spec, _)) => spec,
        None => return false,
    };
    let mut parts = spec.split_whitespace();
    matches!(parts.next(), Some(p) if p.starts_with('-') && is_hunk_range(&p[1..]))
        && matches!(parts.next(), Some(p) if p.starts_with('+') && is_hunk_range(&p[1..]))
        && parts.next().is_none()
}

fn is_hunk_range(spec: &str) -> bool {
    let (start, count) = match spec.split_once(',') {
        Some((start, count)) => (start, Some(count)),
        None => (spec, None),
    };
    start.parse::<u32>().is_ok() && count.is_none_or(|c| c.parse::<u32>().is_ok())
}

fn parse_diff_and_log(text: &str, stream: Stream) -> Result<Doc, ParseError> {
    let mut doc = Doc::empty(stream);
    let mut current: Vec<Line> = Vec::new();
    let mut kind = Kind::Plain;
    let mut file = String::new();
    let oneline = looks_like_oneline_stream(text);

    for (index, raw) in text.lines().enumerate() {
        let origin = index + 1;

        if oneline && is_oneline_log(raw) {
            flush(&mut doc, &mut current, &mut kind)?;
            kind = Kind::Header;
            current.push(Line { text: raw.to_string(), origin });
            continue;
        }

        if is_commit_line(raw) {
            flush(&mut doc, &mut current, &mut kind)?;
            kind = Kind::Header;
            current.push(Line { text: raw.to_string(), origin });
            continue;
        }

        if raw.starts_with("diff --git ") {
            flush(&mut doc, &mut current, &mut kind)?;
            file = file_from_diff_git(raw);
            kind = Kind::Diff { file: file.clone(), hunk: None };
            current.push(Line { text: raw.to_string(), origin });
            continue;
        }

        if raw.starts_with("@@") {
            if !is_hunk_header(raw) {
                return Err(ParseError { message: format!("unexpected hunk header: {raw}") });
            }
            flush(&mut doc, &mut current, &mut kind)?;
            kind = Kind::Diff { file: file.clone(), hunk: Some(raw.to_string()) };
            current.push(Line { text: raw.to_string(), origin });
            continue;
        }

        if raw.starts_with("+++ b/") {
            file = raw.trim_start_matches("+++ b/").to_string();
        }

        current.push(Line { text: raw.to_string(), origin });
    }

    flush(&mut doc, &mut current, &mut kind)?;
    if doc.blocks.is_empty() {
        return Err(ParseError { message: "no git structure recognized".into() });
    }
    Ok(doc)
}

fn looks_like_oneline_stream(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    !lines.is_empty() && lines.iter().all(|l| is_oneline_log(l))
}

fn file_from_diff_git(line: &str) -> String {
    let rest = line.strip_prefix("diff --git ").unwrap_or(line);
    let Some((_, new)) = rest.rsplit_once(' ') else {
        return rest.to_string();
    };
    new.strip_prefix("b/").unwrap_or(new).to_string()
}

fn parse_status(text: &str, stream: Stream) -> Result<Doc, ParseError> {
    let mut doc = Doc::empty(stream);
    let mut current: Vec<Line> = Vec::new();
    let mut kind = Kind::Plain;

    for (index, raw) in text.lines().enumerate() {
        let origin = index + 1;

        if is_status_header(raw) {
            flush(&mut doc, &mut current, &mut kind)?;
            kind = Kind::Header;
            current.push(Line { text: raw.to_string(), origin });
            continue;
        }

        if is_porcelain(raw) {
            flush(&mut doc, &mut current, &mut kind)?;
            let file = porcelain_path(raw);
            kind = Kind::Diff { file, hunk: None };
            current.push(Line { text: raw.to_string(), origin });
            continue;
        }

        if let Some(file) = human_status_path(raw) {
            flush(&mut doc, &mut current, &mut kind)?;
            kind = Kind::Diff { file, hunk: None };
            current.push(Line { text: raw.to_string(), origin });
            continue;
        }

        if raw.trim().is_empty() {
            flush(&mut doc, &mut current, &mut kind)?;
            continue;
        }

        if current.is_empty() {
            kind = Kind::Header;
        }
        current.push(Line { text: raw.to_string(), origin });
    }

    flush(&mut doc, &mut current, &mut kind)?;
    if doc.blocks.is_empty() {
        return Err(ParseError { message: "empty status".into() });
    }
    Ok(doc)
}

fn is_status_header(line: &str) -> bool {
    line.starts_with("On branch ")
        || line.starts_with("HEAD detached ")
        || line == "Not currently on any branch."
        || line.starts_with("Changes to be committed:")
        || line.starts_with("Changes not staged for commit:")
        || line.starts_with("Untracked files:")
        || line.starts_with("Unmerged paths:")
        || line.starts_with("Your branch ")
        || line.starts_with("nothing to commit")
}

fn human_status_path(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    for prefix in [
        "modified: ",
        "new file: ",
        "deleted: ",
        "renamed: ",
        "copied: ",
        "both modified: ",
        "both added: ",
        "deleted by us: ",
        "deleted by them: ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let path = rest.rsplit_once(" -> ").map(|(_, n)| n).unwrap_or(rest).trim();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    if line.starts_with('\t') && !trimmed.starts_with('(') {
        let path = trimmed.rsplit_once(" -> ").map(|(_, n)| n).unwrap_or(trimmed).trim();
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }
    None
}

fn porcelain_path(line: &str) -> String {
    let rest = line.get(3..).unwrap_or(line);
    rest.rsplit_once(" -> ").map(|(_, n)| n).unwrap_or(rest).trim().to_string()
}

fn flush(doc: &mut Doc, current: &mut Vec<Line>, kind: &mut Kind) -> Result<(), ParseError> {
    if current.is_empty() {
        return Ok(());
    }
    if current.iter().all(|l| l.text.trim().is_empty()) {
        current.clear();
        return Ok(());
    }
    let mut block = Block::new(std::mem::take(current));
    block.kind = std::mem::take(kind);
    doc.blocks.push(block);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(text: &str) -> Doc {
        parse(text, Stream::Stdout).expect("parse")
    }

    #[test]
    fn a_hunk_header_is_kept_verbatim() {
        let header = "@@ -12,7 +12,8 @@ pub fn parse()";
        let text = format!(
            "diff --git a/src/lib.rs b/src/lib.rs\n\
             --- a/src/lib.rs\n\
             +++ b/src/lib.rs\n\
             {header}\n\
              keep\n\
             -old\n\
             +new\n"
        );
        let doc = parse_ok(&text);
        let hunks: Vec<&str> = doc
            .blocks
            .iter()
            .filter_map(|b| match &b.kind {
                Kind::Diff { hunk: Some(h), .. } => Some(h.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(hunks, vec![header]);
        assert!(doc.blocks.iter().any(|b| b.lines.iter().any(|l| l.text == header)));
    }

    #[test]
    fn a_malformed_hunk_header_is_an_error() {
        let text = "diff --git a/f b/f\n@@ not a hunk @@\n+x\n";
        let err = parse(text, Stream::Stdout).unwrap_err();
        assert!(err.message.contains("unexpected hunk header"), "{err}");
    }

    #[test]
    fn each_hunk_is_its_own_block() {
        let text = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,1 +1,1 @@
-a
+b
@@ -10,1 +10,1 @@
-c
+d
";
        let doc = parse_ok(text);
        let hunks = doc
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, Kind::Diff { hunk: Some(_), .. }))
            .count();
        assert_eq!(hunks, 2);
    }

    #[test]
    fn a_commit_header_is_kind_header() {
        let text = "\
commit abcdef0
Author: A <a@b>
Date:   Fri Aug 21 00:00:00 2026 +0000

    subject

diff --git a/f b/f
--- a/f
+++ b/f
@@ -1 +1 @@
-a
+b
";
        let doc = parse_ok(text);
        assert!(matches!(doc.blocks[0].kind, Kind::Header));
        assert!(doc.blocks.iter().any(|b| matches!(b.kind, Kind::Diff { hunk: Some(_), .. })));
    }

    #[test]
    fn status_paths_are_diff_blocks() {
        let text = "\
On branch main
Changes not staged for commit:
  (use \"git add <file>...\" to update what will be committed)
	modified:   src/adapters/git.rs

Untracked files:
	scratch.rs
";
        let doc = parse_ok(text);
        let files: Vec<String> = doc
            .blocks
            .iter()
            .filter_map(|b| match &b.kind {
                Kind::Diff { file, .. } => Some(file.clone()),
                _ => None,
            })
            .collect();
        assert!(files.iter().any(|f| f == "src/adapters/git.rs"), "{files:?}");
        assert!(files.iter().any(|f| f == "scratch.rs"), "{files:?}");
        assert!(matches!(doc.blocks[0].kind, Kind::Header));
    }

    #[test]
    fn porcelain_status_is_one_block_per_path() {
        let text = "M  staged.rs\n M unstaged.rs\n?? new.rs\n";
        let doc = parse_ok(text);
        assert_eq!(doc.blocks.len(), 3);
        match &doc.blocks[0].kind {
            Kind::Diff { file, hunk } => {
                assert_eq!(file, "staged.rs");
                assert!(hunk.is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn oneline_log_is_one_header_per_commit() {
        let text = "abcdef0 subject one\n1234567 subject two\n";
        let doc = parse_ok(text);
        assert_eq!(doc.blocks.len(), 2);
        assert!(doc.blocks.iter().all(|b| matches!(b.kind, Kind::Header)));
    }

    #[test]
    fn prose_is_not_git_output() {
        let err = parse("hello\nworld\n", Stream::Stdout).unwrap_err();
        assert!(err.message.contains("not git"), "{err}");
    }

    #[test]
    fn origins_are_the_raw_line_numbers() {
        let text = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n";
        let doc = parse_ok(text);
        let origins: Vec<usize> =
            doc.blocks.iter().flat_map(|b| &b.lines).map(|l| l.origin).collect();
        assert!(origins.windows(2).all(|w| w[0] < w[1]), "{origins:?}");
        assert_eq!(origins[0], 1);
    }

    #[test]
    fn the_large_show_fixture_preserves_every_hunk_header() {
        let text = include_str!("../../tests/fixtures/git_show_large.txt");
        let doc = parse_ok(text);
        let raw_headers: Vec<&str> = text.lines().filter(|l| l.starts_with("@@ ")).collect();
        assert!(!raw_headers.is_empty(), "fixture should contain hunks");
        for header in &raw_headers {
            assert!(
                is_hunk_header(header),
                "fixture header not recognized as unified diff: {header}"
            );
            let found = doc.blocks.iter().any(|b| match &b.kind {
                Kind::Diff { hunk: Some(h), .. } => h == header,
                _ => b.lines.iter().any(|l| &l.text == header),
            });
            assert!(found, "lost hunk header: {header}");
        }
    }
}
