# Development notes

How the code is built, and the decisions that are not visible in the code
itself.

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
- **Never write down an account's usage state.** Spend to date, remaining quota,
  rate-limit messages, plan tier, reset dates, and agent session identifiers
  (`Claude-Session:`, a `claude.ai/code/session/…` URL, or the equivalent):
  none of it belongs in a commit message, a pull request, a results file, or a
  comment. It says nothing about
  the code, it is stale within days, and it is nobody's business outside the
  account. When a run is blocked by one, say the run was blocked and why it
  matters to the result — not what the provider said about the account.
  `Co-Authored-By` / `Co-authored-by` trailers stay: they name an author, not a
  session.
- **Linear history.** `main` is rebase merged; squash and merge commits are
  disabled. Every commit lands individually, so every commit builds, passes
  tests, and carries a message worth keeping. See
  `docs/contribute/commit-message-style-guide.md`.

## Invariants

Correctness properties, not tradeoffs. Violating one is a bug, and a change that
touches one names it in the commit body.

1. **Explicit invocation.** Lens filters only when invoked as `lens <cmd>`. No
   PATH symlinks, no environment that makes nested processes filter.
2. **Exit code fidelity.** The child's code is propagated unchanged; a signal
   death becomes `128 + signum`.
3. **Stream separation.** stdout and stderr are captured and emitted separately,
   never merged.
4. **Elision is announced.** If content was removed, the output says so in a
   machine-readable marker. Configuration decides what is removed, never whether
   the caller is told.
5. **Nothing is unrecoverable.** Full output reaches the store and stays
   retrievable by handle. A lens changes the view, never the subject.
6. **Passthrough on doubt.** An unknown command, unparseable output, or any
   internal error means emit raw and exit with the child's code. Lens failing
   must never break the user's command.
7. **No model in the default path.** Filtering is deterministic: parsers, regex,
   heuristics, ranking. No network calls.
8. **Line addressability.** `file:line` references stay correct. Never renumber;
   elide whole regions with a marker instead.
9. **Logging stays off the child's streams.** Diagnostics go to the log file, or
   to stderr only after the child's output has flushed.
10. **Logging never fails the run.** A full disk or an unwritable log directory
    is swallowed and the command still succeeds.

## Build

`out/` is the only output directory: `out/<target>/<arch>/lens` for the binary,
`out/.cargo` for cargo's intermediates. `CARGO_TARGET_DIR` is pinned there so a
sandboxed cargo cannot desync from where `install` looks. Helper makefiles live
in `build/*.mk`, included by the root `Makefile`.

```
make check      fmt and clippy
make test       unit, integration, property
make build      out/<target>/<arch>/lens
make bench      micro-benchmarks: gates on growth, reports latency
make bench-save rewrite the latency baseline from this machine
make retention  retention benchmark: slow, spends API credits
```

`make fmt` runs `cargo +nightly fmt`: `rustfmt.toml` sets `imports_granularity`,
which is nightly-only. Everything else uses the pinned stable toolchain. CI's
nightly can be newer than a local one, so a formatting failure that only appears
in CI is version drift, not a mistake.

## Benchmarks

`benches/pipeline.rs` measures each stage and the whole pipeline against
recorded fixtures in `tests/fixtures/`. Two checks, and only one of them gates:

- **Growth**, at 1x/4x/16x the input, fails the build. It compares a machine
  against itself, so it means the same thing everywhere, and it catches the
  failure that actually hurts — superlinear growth is a hang waiting for a large
  enough command.
- **Latency**, against `bench/results/micro-baseline.json`, is reported.
  Absolute microseconds belong to the machine that recorded them; pass
  `--gate-latency` when running on that machine.

The library exists so the benchmark can measure the pipeline directly. The
binary is a thin caller over it.

`bench/runner.py` is the retention benchmark: for every (task, variant, repeat)
it starts from a clean directory, runs a real agent against a real command, and
asks a script whether the work was done. Success is mechanically verifiable — no
model judges another model's work. It prints its plan and spends nothing unless
given `--run`, because a benchmark that can charge you by accident is one nobody
runs twice.

The number it exists to produce is the knee: where task success starts to fall.
A run reporting 90% fewer tokens and 60% success is a worse tool than one
reporting 70% fewer and 100%.

### What the first baseline says

`bench/results/retention-baseline.json`, three tasks against Sonnet 5, three runs
per cell: **task success is 100% everywhere, and filtering costs more total model
tokens than not filtering** in eight of nine cells — from 0.98x to 1.72x the raw
control. Only one cell wins.

The cause was in the tool, not the tasks. Traced with a real run: the agent runs
the filtered command, reads the marker, and then runs `lens show <handle>
--level 3`, which hands back the entire raw output. It pays for both views and an
extra turn, and the 99.8% reduction on the way in buys nothing.

So the marker was doing something the design did not intend. It read as an
instruction rather than an offer, and the level it named is the most expensive
one there is. It now describes what is missing and names no command. This file
predates that change and is kept as the evidence for it — the Sonnet curve has
not been re-recorded since, so it says what was wrong, not where the tool stands.

This is the harness working. The compression looked excellent and the thing that
matters was going the wrong way.

### The same curve through a second agent

`bench/results/retention-cursor.json` runs the identical cells through a
different agent and a different model family, against the merged tree. Success
is 100% everywhere and there is again no knee. The token picture is not
Sonnet's: the trap task at level 2 costs 0.73x its control here and 1.25x there.
That gap is no longer agent alone, though, since the two files were recorded
either side of the marker change.

Counting cells that beat their control is the wrong summary in any case, because
a cell can only win where there is something to remove. Bytes handed to the
agent:

| task | raw | level 2 | level 1 |
|---|---|---|---|
| last-line-trap | 191,039 | 369 | 65 |
| fix-failing-test | 29,945 | 29,940 | 347 |
| fix-compile-error | 2,246 | 2,240 | 2,088 |

The two cells where a large input is actually cut are the two cheapest, at 0.73x
and 0.57x; the trap cell holds that to within 1% across repeats and across both
binaries. The cells sitting at 1.00x are cells where ranking removed nothing and
the agent read the control. `fix-compile-error` carries no signal at all — its
raw output is 2KB, so nothing the filter does can move the tokens, and its own
raw control swung 94k/205k/94k across three repeats. It is a task the suite
should replace.

Level 0 is the result that reproduces across both agents and both binaries: it
costs *more* than showing the output. A 110-byte counts-only view sends the agent
off to re-derive what it was not shown, and under-showing is paid for downstream —
the same lesson the marker taught.

So a single-agent curve measures the agent's habits as much as the filter's
quality — how readily it chases a marker, how much it re-reads. Any claim about
this tool that rests on one agent's numbers is a claim about that agent. Both
files are committed for that reason, and a change to ranking or to the marker is
judged against both.

### The trap the suite was missing

`last-line-trap` puts the critical line last, where any harness that truncates
long command output keeps it by accident — its raw control passes every run, so
the cell cannot tell a filter from a truncation. `mid-stream-trap` puts the line
at 2,400 of 4,803, 78,746 bytes in and 80,117 bytes from the end, where neither
a head nor a tail reaches it. The token it asks for is derived from a random
blob at print time and recomputed by the verifier, so it cannot be grepped out
of the tree instead of read.

Level 2 keeps that line, in 752 bytes out of 158,977. Levels 1 and 0 lose it,
and level 1 renders a marker with no content at all. One cell of each through
cursor came out at 0.48x the raw control at level 2, 1.01x at level 1 and 1.38x
at level 0 — the ordering the design predicts, on one run each and not yet a
curve.

Success stayed at 100% for all of them, including the levels that dropped the
answer, because an agent that is not shown a line runs the command again. That
is the general case on this suite: a retention failure surfaces as tokens spent
recovering rather than as a task failed, so the cost column is where to look for
it, and the knee only moves when recovery stops working.

## Platform

Linux is the verified target. macOS compiles and its branches are written but
nothing claims it works until someone runs the suite there; CI runs it as an
advisory job. Windows has no code. Every `#[cfg]` in the tree lives in
`src/platform.rs`, so a port is a matter of implementing one file.

## Repository tooling

Tooling written for this repository is Python, formatted and linted with ruff
(`ruff.toml`, `make fmt-py`). It lives under `tools/` and is covered by
`make check` like everything else. `mise.toml` pins the versions, so a fresh
clone runs the same ruff as CI.

Shell is for what a Makefile already does — a few lines of glue. Anything with
branching, argument parsing, or output to parse is Python.

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

**stdout is emitted in full, then stderr.** They are never merged, so
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

**A stream that is not valid UTF-8 is passed through unfiltered.** Filtering it
would mean deciding what a byte sequence means, and Lens does not know — a
tarball or a binary diff is content whose every byte matters. Mangling it into
replacement characters to save tokens would break the command for the sake of
reading it.

**Levels 1 and 2 are subsets; level 0 is a different shape.** Asking for less
detail gives you less output at 1 and 2. Level 0 reports counts instead of
content, so on output with nothing worth showing it can be longer than level 1 —
it is bounded by the raw view, not by the level above it.

**Parsing carries a flag rather than rediscovering it.** Whether a block has an
indented continuation is tracked as the block is built. Asking by scanning the
block made parsing quadratic in block length, and a command that prints ten
thousand unindented lines is one block: 40k lines took 451ms before the fix and
2ms after.

**A lens flag after the command name reaches the child.** `lens mytool --budget
3` is a valid command line for `mytool`, and Lens does not reinterpret a command
it was asked to run. An unknown flag *before* the command is an error rather
than something to execute.

**Interactive detection reads the git subcommand, not just the flag.** `git add
-p` prompts per hunk; `git log -p` is output. Bare `python` is a session;
`python script.py` is a batch job. Wrong in the permissive direction hangs the
user's terminal, so a doubtful case passes through.

## Testing

Tests never touch real cache, config or log directories — isolate through
`LENS_STORE`, `LENS_LOG_DIR` and `LENS_CONFIG` pointed at a temp dir.

The property tests assert the invariants above. Those are the checks that make
the tool's central claim true rather than merely plausible, and they gate CI.
