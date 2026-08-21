// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Token estimation.
//!
//! Budget decisions need a number, not a tokenizer. A BPE tokenizer would cost a
//! dependency carrying model data, tens of milliseconds on a large input, and a
//! coupling to whichever model is current — to answer a question where being
//! within a few percent is indistinguishable from being exact.
//!
//! The trait stays so an exact implementation can be dropped in later, when
//! something needs one.

/// Estimates how many tokens a string will cost.
pub trait TokenEstimator {
    /// Tokens `text` is expected to occupy.
    fn estimate(&self, text: &str) -> usize;
}

/// The default estimator: alphanumeric runs plus the punctuation that splits.
///
/// A BPE vocabulary merges letters into subwords but rarely merges a separator
/// with its neighbour. So a run of letters or digits costs roughly its length
/// over five, and each separator costs one on its own — which is why a path and
/// a sentence of the same length are nowhere near the same price.
#[derive(Debug, Clone, Copy, Default)]
pub struct Heuristic;

/// Characters a tokenizer almost always emits on their own.
const SPLITTERS: &[char] = &[
    '/', '\\', ':', ';', '|', '=', '<', '>', '{', '}', '[', ']', '(', ')', '#', '@', '$', '%', '^',
    '&', '*', '+', '~', '`', '"',
];

/// Characters in a run before it costs another token.
const RUN_PER_TOKEN: usize = 5;

impl TokenEstimator for Heuristic {
    fn estimate(&self, text: &str) -> usize {
        let mut tokens = 0usize;
        let mut run = 0usize;

        for c in text.chars() {
            if c.is_alphanumeric() {
                run += 1;
                continue;
            }

            tokens += run.div_ceil(RUN_PER_TOKEN);
            run = 0;

            if SPLITTERS.contains(&c) {
                tokens += 1;
            }
            // Everything else — spaces, commas, full stops, hyphens — usually
            // merges into a neighbouring token and costs nothing on its own.
        }

        tokens + run.div_ceil(RUN_PER_TOKEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How far the estimate is from `actual`, as a fraction.
    fn error(estimate: usize, actual: usize) -> f64 {
        (estimate as f64 - actual as f64).abs() / actual as f64
    }

    #[test]
    fn empty_text_costs_nothing() {
        assert_eq!(Heuristic.estimate(""), 0);
    }

    #[test]
    fn real_output_lands_within_fifteen_percent() {
        // Counts on the right are what a BPE tokenizer reports for these
        // strings. The target is 15%: closer than that is luck, further makes
        // budget decisions unreliable.
        let cases = [
            ("The quick brown fox jumps over the lazy dog.", 10),
            ("Compiling lens v0.1.0 (/home/user/projects/lens)", 17),
            ("error[E0308]: mismatched types", 9),
            ("test result: ok. 74 passed; 0 failed; 0 ignored", 16),
            ("src/pipeline/mod.rs:142:9", 11),
        ];
        for (text, actual) in cases {
            let estimate = Heuristic.estimate(text);
            assert!(
                error(estimate, actual) <= 0.15,
                "{text:?}: estimated {estimate}, actual {actual}"
            );
        }
    }

    #[test]
    fn a_path_costs_more_than_prose_of_the_same_length() {
        // The case a byte count gets wrong: a path is mostly separators, and
        // each one is its own token. Estimating it as prose would let a budget
        // admit roughly twice the output it can afford.
        let path = "src/pipeline/mod.rs:142:9";
        let prose = "this is a sentence of abc";
        assert_eq!(path.len(), prose.len(), "same length");
        assert!(
            Heuristic.estimate(path) > Heuristic.estimate(prose),
            "path {} vs prose {}",
            Heuristic.estimate(path),
            Heuristic.estimate(prose)
        );
    }

    #[test]
    fn the_estimate_grows_with_the_text() {
        // Budget decisions depend on this and nothing else: more text is never
        // fewer tokens.
        let mut previous = 0;
        let mut text = String::new();
        for _ in 0..50 {
            text.push_str("a line of output\n");
            let estimate = Heuristic.estimate(&text);
            assert!(estimate >= previous, "estimate went backwards");
            previous = estimate;
        }
    }

    #[test]
    fn a_long_input_stays_proportional() {
        let unit = "warning: unused variable `x`\n";
        let one = Heuristic.estimate(unit);
        let hundred = Heuristic.estimate(&unit.repeat(100));
        let ratio = hundred as f64 / one as f64;
        assert!((90.0..=110.0).contains(&ratio), "100x the text estimated {ratio}x the tokens");
    }
}
