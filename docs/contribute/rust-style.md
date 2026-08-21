# Rust style

Formatting is settled by `rustfmt.toml` and enforced by `make fmt`; nothing below
is about whitespace. These are the conventions `make clippy` cannot check.

The baseline is the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/).
Where this document is silent, they apply. Where it is not, it wins.

## Naming

* Casing follows Rust idiom — `C-CASE`.
* Conversions follow `as_` (borrowed → borrowed), `to_` (expensive), `into_`
  (consuming) — `C-CONV`.
* Getters are `handle()`, not `get_handle()` — `C-GETTER`.
* Iterator methods are `iter`, `iter_mut`, `into_iter`, and the types they return
  are named after them — `C-ITER`, `C-ITER-TY`.
* Word order is consistent across the crate — `C-WORD-ORDER`. In this tree the
  vocabulary is fixed by `LENS.md` §Vocabulary: lens, view, handle, elision,
  focus. Use those words in code, config, docs and error messages, and do not
  introduce synonyms for them.

## Types and traits

* Implement `Debug` on everything, and `Copy`, `Clone`, `Eq`, `PartialEq`, `Ord`,
  `PartialOrd`, `Hash`, `Display`, `Default` wherever they make sense —
  `C-COMMON-TRAITS`. A type that cannot be printed cannot be debugged from a log.
* Conversions go through `From`, `AsRef`, `AsMut` — `C-CONV-TRAITS`.
* Error types are meaningful and well behaved — `C-GOOD-ERR`. An error that
  reaches the user says what Lens was doing and what it will do about it, because
  invariant 6 means the answer is almost always "emit raw output and exit with the
  child's code".

## Unsafe

Unsafe is a last resort, confined to `src/platform.rs`, and never used for
performance without a benchmark showing it matters.

**Every `unsafe` block carries a `// SAFETY:` comment** explaining why it is
sound — the preconditions the call requires and why they hold here. A block
without one does not merge.

```rust
// SAFETY: flock(2) takes an open file descriptor and a flag word. `fd` is
// borrowed from `file`, which outlives this call, and LOCK_EX | LOCK_NB is a
// valid operation pair. The call cannot dereference memory, so the only
// failure mode is the errno we check below.
let rc = unsafe { flock(fd, LOCK_EX | LOCK_NB) };
```

Layout and ABI assumptions at an `extern "C"` boundary are asserted at compile
time with `static_assert!`, not documented in prose and not left to a test.

## Documentation

* Every module has a `//!` header saying what it is for — not what it contains.
* Public items are documented, and the docs say *why* the thing exists where the
  name does not already make that obvious — `C-CRATE-DOC`, `C-EXAMPLE`.
* Function docs cover errors, panics and safety requirements — `C-FAILURE`.
* Examples use `?`, never `unwrap()` — `C-QUESTION-MARK`.
* Comments explain reasoning, not mechanics. A comment restating the line below
  it is noise; a comment naming the invariant that line protects is the point.

## Tests

Every module ships `#[cfg(test)] mod tests` in the same file, covering that
module's own logic. Integration and property tests live in `tests/`. See
`LENS.md` §16 for what the property tests must assert — those are the checks that
make the tool's central claim true rather than merely plausible.

Tests never touch the real cache, config or log directories: isolate through
`LENS_STORE`, `LENS_LOG_DIR` and `LENS_CONFIG` pointed at a temp dir.

## Dependencies

`LENS.md` §2 approves `regex`, `serde` + `toml`, and `anyhow`, plus `serde_json`
for the JSONL the log and store need. Everything else needs a justification in
the commit that adds it: what it buys, what it costs in tree size, and what was
rejected instead. Explicitly unwanted: async runtimes, `clap`, tokenizer crates
with model data, plugin runtimes, and the `tracing`/`log` subscriber stack.
