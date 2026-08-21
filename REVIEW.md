# Review instructions

This tool sits between a command and the agent reading its output. Its failure
mode is not a crash — it is a view that looks complete and is not. Weight the
review accordingly.

## What Important means here

Reserve 🔴 Important for a change that could make Lens misrepresent a command:

- A failing command whose output no longer shows the failure. This is the worst
  bug the tool can have: the reader concludes the command succeeded.
- Content removed without an elision marker, or removed with no way to recover
  it from the store.
- A wrong exit code, merged streams, mutated `file:line` references, or renumbered
  lines.
- Output altered on a path that claims to be byte-identical — passthrough, raw
  mode, or the raw view of a stored run.
- A hang: an unread pipe, a lock held across a wait, an unbounded read.
- A handle or path from outside the process used without validation.
- A Lens failure that becomes the user's failure. Log, store, and config errors
  are swallowed by design; a new `?` or `unwrap` on those paths is a bug.

Performance is Important only with a measurement. This tool has a ~10ms budget,
so a change that plausibly costs milliseconds on a large input is worth flagging —
as a Nit unless a benchmark shows it.

Style, naming, and structure are 🟡 Nit at most.

## Always check

- A commit touching one of the invariants in `AGENTS.md` names it in the body. A
  commit under `src/pipeline/` or `src/adapters/` carries a `Test:` footer with
  a real result, not "tests pass".
- Every new module has `#[cfg(test)] mod tests` covering its own logic.
- Every `unsafe` block has a `// SAFETY:` comment that states the precondition
  and why it holds. Any `#[cfg]` outside `src/platform.rs` is a finding.
- A new dependency is justified in the commit that adds it. Async runtimes,
  argument parsers, tokenizer crates, and logging frameworks are out of scope by
  decision, not oversight.
- A stage marks blocks; it does not delete them, and it does not mutate
  `Line::origin`. Removal happens once, in the renderer.
- Test state goes through `LENS_STORE`, `LENS_LOG_DIR` or `LENS_CONFIG` into a
  temp directory. A test that touches a real cache, config or log directory is
  Important.
- Speculative generality: an abstraction with one caller, a parameter no caller
  varies, a trait implemented once. Say which caller would need it.

## Do not report

- Anything CI already enforces: formatting, clippy lints, compile errors.
- `out/`, `Cargo.lock`, and anything generated.
- Missing rustdoc on private items, or absent test coverage for code the same
  commit deletes.
- Requests to handle a platform the tree does not target. macOS branches are
  written but unverified by decision; Windows is absent by decision.

## Verification bar

A behavior claim needs a `file:line` citation, not an inference from a name. For
a concurrency or ordering claim, name the interleaving. Reporting a bug that
turns out to hold only in theory costs the author a round trip and teaches them
to skim the next review.

## Volume and repeat reviews

Report at most five Nits per review; if there are more, give a count in the
summary. After the first review of a pull request, post Important findings only.

Open the summary with a tally — `2 important, 4 nits` — and lead with "no
blocking issues" when that is the case.
