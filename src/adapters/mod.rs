// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Turning raw bytes into a document.
//!
//! An adapter is opt-in refinement. A command nobody wrote an adapter for still
//! gets the full pipeline through [`generic`], which is why there is no such
//! thing as output Lens cannot filter — only output it cannot filter *well*.

pub mod generic;
pub mod git;

use crate::pipeline::{Doc, Stream};

/// Parse raw stream bytes into a document with the generic adapter.
pub fn parse(raw: &[u8], stream: Stream) -> Doc {
    parse_with(raw, stream, "generic").0
}

/// Parse with a named adapter.
///
/// `git` is the only structured adapter. Anything else, and any git parse
/// failure, uses [`generic`]. The optional string is the fallback reason, for
/// the warn that belongs in the log rather than on the child's streams.
pub fn parse_with(raw: &[u8], stream: Stream, adapter: &str) -> (Doc, Option<String>) {
    let text = String::from_utf8_lossy(raw);
    if adapter == "git" {
        match git::parse(&text, stream) {
            Ok(doc) => (doc, None),
            Err(err) => (generic::parse(&text, stream), Some(err.to_string())),
        }
    } else {
        (generic::parse(&text, stream), None)
    }
}
