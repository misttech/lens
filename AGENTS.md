# Development notes

`LENS.md` is the spec: what Lens does and why. It is deliberately kept out of
this repository, so the section and invariant numbers cited here and in the
source comments refer to that document rather than to anything in the tree.

This file says how the code is built, and records decisions that are not
visible in the code itself.

## Working rules

- **YAGNI.** Build what the current milestone needs. No abstraction for a second
  implementation that does not exist; a trait earns its place at the second
  caller, not the first. `cargo clippy -- -D warnings` enforces this in practice —
  code written a milestone early shows up as dead.
- **Every module ships `#[cfg(test)] mod tests`** in the same file, covering that
  module's own logic. Integration and property tests live in `tests/`.
- **Every `unsafe` block carries a `// SAFETY:` comment** explaining why it is
  sound. Unsafe is confined to `src/platform.rs`.
- **Layout and ABI assumptions are asserted at compile time**, with
  `static_assert!`, not documented in prose and not left to a test.
- **Linear history.** `main` is rebase merged; squash and merge commits are
  disabled. Every commit lands individually, so every commit builds, passes
  tests, and carries a message worth keeping. See
  `docs/contribute/commit-message-style-guide.md`.

## Build

`out/` is the only output directory: `out/<target>/<arch>/lens` for the binary,
`out/.cargo` for cargo's intermediates. `CARGO_TARGET_DIR` is pinned there so a
sandboxed cargo cannot desync from where `install` looks. Helper makefiles live
in `build/*.mk`, included by the root `Makefile`.

```
make check      fmt and clippy
make test       unit, integration, property
make build      out/<target>/<arch>/lens
make bench      micro-benchmarks
make retention  retention benchmark: slow, spends API credits
```

`make fmt` runs `cargo +nightly fmt`: `rustfmt.toml` sets `imports_granularity`,
which is nightly-only. Everything else uses the pinned stable toolchain. CI's
nightly can be newer than a local one, so a formatting failure that only appears
in CI is version drift, not a mistake.

## Platform

Linux is the verified target. macOS compiles and its branches are written but
nothing claims it works until someone runs the suite there; CI runs it as an
advisory job. Windows has no code. Every `#[cfg]` in the tree lives in
`src/platform.rs`, so a port is a matter of implementing one file.

## Dependencies

Approved: `regex`, `serde` + `serde_json` + `toml`, `anyhow`. Anything else needs
a justification in the commit that adds it — what it buys, what it costs, what
was rejected instead. Explicitly unwanted: async runtimes, `clap`, tokenizer
crates carrying model data, plugin runtimes, and the `tracing`/`log` subscriber
stack.

## Decisions

Things a reader would otherwise have to reconstruct.

**Passthrough uses `exec`.** Replacing the process image is the only way to be
byte-identical: same stdio, same terminal control, same signal disposition, with
Lens no longer present. Spawning and copying streams is an imitation.

**Capture reads each stream on its own thread.** One reader deadlocks as soon as
the unread pipe fills. This is also why there is no async runtime.

**stdout is emitted in full, then stderr.** Invariant 3 forbids merging them, so
interleaving relative to a terminal run is lost. The gain is that stages can tell
which stream a line came from, which is what lets a failing command's stderr be
force-kept.

**Handles use an in-tree FNV-1a, not `DefaultHasher`.** The standard hasher's
algorithm is explicitly not stable across releases, and a handle printed
yesterday has to still resolve after a toolchain upgrade. It is checked against
published test vectors, because a hash that is wrong in a *stable* way passes
every round-trip test you can write for it.

**Handles from users are parsed, not trusted.** Eight lowercase hex digits or
nothing, so a handle cannot become a path traversal.

**`serde_json` is in the tree despite the spec preferring a hand-written
serializer.** The write side would be fine hand-rolled; `lens stats` has to parse
the log back and `lens show` has to parse `meta.json`, and a hand-rolled parser
is the larger liability.

**A passthrough run record omits `exit`, `dur_ms` and byte counts.** `exec`
replaces this process, so the record is written before the command runs and there
is no outcome to report. A placeholder would put fabricated exit codes into
`lens stats`.

**argv is logged; command output is not.** The log cannot answer what was run
without argv, so a secret passed in an argument does reach the log. Output never
does below `trace`, where it is capped at a short prefix.

**A lens flag after the command name reaches the child.** `LENS.md` §3 reads as
rejecting it, but `lens mytool --budget 3` is a valid command line for `mytool`,
and Lens does not reinterpret a command it was asked to run. An unknown flag
*before* the command is an error rather than something to execute.

**Interactive detection reads the git subcommand, not just the flag.** `git add
-p` prompts per hunk; `git log -p` is output. Bare `python` is a session;
`python script.py` is a batch job. Wrong in the permissive direction hangs the
user's terminal, so a doubtful case passes through.

## Testing

Tests never touch real cache, config or log directories — isolate through
`LENS_STORE`, `LENS_LOG_DIR` and `LENS_CONFIG` pointed at a temp dir.

`LENS.md` §16 lists the property tests. Those are the checks that make the tool's
central claim true rather than merely plausible, and they gate CI.
