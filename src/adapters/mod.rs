// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Turning raw bytes into a document.
//!
//! An adapter is opt-in refinement. A command nobody wrote an adapter for still
//! gets the full pipeline through [`generic`], which is why there is no such
//! thing as output Lens cannot filter — only output it cannot filter *well*.

pub mod generic;

use crate::pipeline::{Doc, Stream};

/// Parse raw stream bytes into a document.
///
/// Bytes, not text: command output is not guaranteed to be UTF-8, and the raw
/// view has to be byte-identical to what the command produced. Invalid
/// sequences are replaced for the purpose of *filtering* only — the store still
/// holds the original, and that is what the raw view reads.
pub fn parse(raw: &[u8], stream: Stream) -> Doc {
    generic::parse(&String::from_utf8_lossy(raw), stream)
}
