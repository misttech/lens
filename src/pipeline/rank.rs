// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Score every block so the budget stage has an order to drop in.
//!
//! Rank does not drop anything. It writes a number in `[0, 1]` onto each block
//! so that, under pressure, the least useful content goes first and a failure
//! is the last thing standing. The number is a ranking, not a measurement: an
//! error at 1.0 is not "twice as important" as a warning at 0.6, it is simply
//! never dropped before one.

use std::collections::{HashMap, HashSet};

use super::{Class, Ctx, Doc, Stage, Stream};

/// The rank stage.
#[derive(Debug, Clone, Copy)]
pub struct Rank;

/// Base score by class, before modifiers.
fn base(class: Class) -> f32 {
    match class {
        Class::Error | Class::Failure => 1.0,
        Class::Warning => 0.6,
        Class::Info => 0.3,
        Class::Progress => 0.05,
        Class::Noise => 0.0,
    }
}

/// How much a vendored path is worth deprioritizing.
const VENDORED: f32 = 0.25;
/// How much a path shared with another kept block is worth keeping.
const REFERENCED: f32 = 0.1;
/// How much stderr of a failing command is worth keeping.
const FAILING_STDERR: f32 = 0.15;

impl Stage for Rank {
    fn name(&self) -> &'static str {
        "rank"
    }

    fn apply(&self, doc: &mut Doc, ctx: &Ctx) {
        let referenced = referenced_paths(doc);
        let failing_stderr = doc.source == Stream::Stderr && ctx.exit_code != 0;

        for block in &mut doc.blocks {
            let mut score = base(block.class);

            // A failure in a lockfile is still a failure. Vendored paths
            // deprioritize noise from generated trees, not the errors in them.
            if !matches!(block.class, Class::Error | Class::Failure)
                && block.lines.iter().any(|line| is_vendored(&line.text))
            {
                score -= VENDORED;
            }

            if !referenced.is_empty()
                && block.lines.iter().any(|line| {
                    paths_in(&line.text).iter().any(|path| referenced.contains(path.as_str()))
                })
            {
                score += REFERENCED;
            }

            if failing_stderr {
                score += FAILING_STDERR;
            }

            block.score = score.clamp(0.0, 1.0);
        }
    }
}

/// Paths that appear in at least two kept blocks.
///
/// A file mentioned next to an error and again in a hunk of context is the
/// same file, and the second mention is how the reader gets there.
fn referenced_paths(doc: &Doc) -> HashSet<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for block in doc.blocks.iter().filter(|b| b.kept()) {
        let mut seen: HashSet<String> = HashSet::new();
        for line in &block.lines {
            for path in paths_in(&line.text) {
                if seen.insert(path.clone()) {
                    *counts.entry(path).or_insert(0) += 1;
                }
            }
        }
    }
    counts.into_iter().filter(|(_, n)| *n >= 2).map(|(p, _)| p).collect()
}

/// File-like tokens: a slash, or a name with a source extension, optionally
/// followed by `:line`.
fn paths_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for token in
        text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '[' | ']'))
    {
        let token = token.trim_end_matches(':');
        if token.is_empty() {
            continue;
        }
        let path = token.split(':').next().unwrap_or(token);
        if is_path(path) && !found.iter().any(|p| p == path) {
            found.push(path.to_string());
        }
    }
    found
}

fn is_path(token: &str) -> bool {
    if token.contains('/') {
        return true;
    }
    const EXTS: &[&str] = &[".rs", ".c", ".h", ".py", ".js", ".ts", ".go", ".java", ".toml"];
    EXTS.iter().any(|ext| token.ends_with(ext))
}

/// Lockfiles, build products, and minified assets — content generated rather
/// than written, and the first thing a budget should give up.
fn is_vendored(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "node_modules/",
        "/target/",
        "\ntarget/",
        "/dist/",
        "\ndist/",
        ".min.",
        "cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "go.sum",
        "poetry.lock",
        "gemfile.lock",
    ];
    MARKERS.iter().any(|m| lower.contains(m))
        || lower.starts_with("target/")
        || lower.starts_with("dist/")
}

#[cfg(test)]
mod tests {
    use super::super::tests::doc_of;
    use super::super::{Class, Keep, Stage, classify, context};
    use super::*;

    fn ranked(lines: &[&str], exit_code: i32) -> Doc {
        let mut doc = doc_of(lines);
        let ctx = Ctx { exit_code, ..Ctx::default() };
        classify::Classify.apply(&mut doc, &ctx);
        context::Context.apply(&mut doc, &ctx);
        Rank.apply(&mut doc, &ctx);
        doc
    }

    #[test]
    fn class_sets_the_base_score() {
        let doc = ranked(&["error: boom", "warning: hm", "ordinary", "---"], 0);
        assert_eq!(doc.blocks[0].score, 1.0);
        assert_eq!(doc.blocks[1].score, 0.6);
        assert_eq!(doc.blocks[2].score, 0.3);
        assert_eq!(doc.blocks[3].score, 0.0);
    }

    #[test]
    fn a_failure_outranks_everything() {
        let doc = ranked(&["ordinary", "error: boom", "warning: hm"], 1);
        let error = doc.blocks.iter().find(|b| b.class == Class::Error).unwrap();
        assert!(
            doc.blocks.iter().filter(|b| b.class != Class::Error).all(|b| b.score < error.score)
        );
    }

    #[test]
    fn a_vendored_info_block_is_cheaper_than_source() {
        let mut doc = doc_of(&["src/main.rs:1: ok", "node_modules/left-pad/index.js:1: ok"]);
        Rank.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks[1].score < doc.blocks[0].score);
    }

    #[test]
    fn vendoring_does_not_deprioritize_a_failure() {
        let doc = ranked(&["error: boom in node_modules/x/index.js"], 1);
        assert_eq!(doc.blocks[0].score, 1.0);
    }

    #[test]
    fn a_path_shared_across_kept_blocks_is_boosted() {
        let mut doc = doc_of(&["src/auth.rs:12: note", "help: see src/auth.rs"]);
        Rank.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks[0].score > base(Class::Info));
        assert!(doc.blocks[1].score > base(Class::Info));
    }

    #[test]
    fn stderr_of_a_failing_command_is_boosted() {
        let mut doc = doc_of(&["a line of output"]);
        doc.source = Stream::Stderr;
        Rank.apply(&mut doc, &Ctx { exit_code: 1, ..Ctx::default() });
        assert!(doc.blocks[0].score > base(Class::Info));
    }

    #[test]
    fn stderr_of_a_successful_command_is_not() {
        let mut doc = doc_of(&["a line of output"]);
        doc.source = Stream::Stderr;
        Rank.apply(&mut doc, &Ctx::default());
        assert_eq!(doc.blocks[0].score, base(Class::Info));
    }

    #[test]
    fn scores_stay_in_unit_interval() {
        let mut doc = doc_of(&["error: boom", "---"]);
        doc.source = Stream::Stderr;
        Rank.apply(&mut doc, &Ctx { exit_code: 1, ..Ctx::default() });
        for block in &doc.blocks {
            assert!((0.0..=1.0).contains(&block.score), "{}", block.score);
        }
    }

    #[test]
    fn rank_does_not_drop_blocks() {
        let mut doc = doc_of(&["one", "two", "three"]);
        Rank.apply(&mut doc, &Ctx::default());
        assert!(doc.blocks.iter().all(|b| b.keep == Keep::Normal));
    }
}
