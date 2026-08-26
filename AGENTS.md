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

### What the Sonnet curve says

`bench/results/retention-baseline.json`, five tasks against Sonnet 5, three runs
per cell, recorded after cause grouping, the classify shapes, excerpt
suppression and the level 1 fix. Success is 100% in all 60 cells and there is no
knee.

| task | raw | level 2 | level 1 | level 0 |
|---|---|---|---|---|
| cargo-cascade | 1.00 | 1.00 | 1.00 | 2.32 |
| cascading-errors | 1.00 | 0.99 | 1.17 | 2.96 |
| fix-failing-test | 1.00 | 0.97 | 0.78 | 1.32 |
| last-line-trap | 1.00 | 0.99 | 1.00 | 1.44 |
| mid-stream-trap | 1.00 | 0.79 | 1.01 | 1.92 |

**Level 2 is free and mostly does nothing.** Four of five tasks land between 0.97
and 1.00 while reading a fraction of the bytes — 1,453 of 49,556 on
`cargo-cascade`, and the session costs the same. The tool calls say why: four
against four, six against six. This agent does the same work either way, so
what the filter saves on the way in is not what the session is made of.

**Level 0 was the worst view in the tree**: 2.32x on `cargo-cascade` with nine
tool calls against four, 2.96x on `cascading-errors` with fifteen against six. A
view of counts alone sent the agent to do the reading itself and it did two to
three times the work. It now carries one line of the output — the failure, or
the last line when nothing failed — and the worst cell went from 2.96x to 0.98x
with the tool calls back to six. The table above predates that change; the three
cells re-measured after it read 0.98, 1.89 and 1.49.

What the two that did not move say is that one line cannot answer every
question. `mid-stream-trap` hides its answer in the middle, where no anchor
reaches it, and the agent runs the command again — which is the same mechanism
level 1 shows on `last-line-trap`, and the reason level 0 still costs more than
raw wherever its one line is not the one wanted.

The one clear win is `mid-stream-trap` at 0.79, where the answer is a single
line in the middle of 4,800 and the filter hands it over directly.

### What the first baseline said

The curve this replaced was recorded before the elision marker stopped naming
`lens show --level 3`. It reported filtering costing more than not filtering in
eight of nine cells, traced to agents reading the marker as an instruction and
fetching the whole raw output. That is the finding the marker change came from,
and the numbers above are what the same agent does now.

### The same curve through a second agent

`bench/results/retention-cursor.json` runs the cells through a different agent
and a different model family: four tasks, four variants, three runs each,
recorded after cause grouping, the classify shapes, excerpt suppression and the
level 1 fix. Success is 100% in all 48 cells and there is no knee.

| task | raw | level 2 | level 1 | level 0 |
|---|---|---|---|---|
| cascading-errors | 1.00 | 0.98 | 0.83 | 1.46 |
| fix-failing-test | 1.00 | 1.00 | 0.59 | 0.71 |
| last-line-trap | 1.00 | 0.71 | 1.25 | 1.29 |
| mid-stream-trap | 1.00 | 0.72 | 0.71 | 1.26 |

Two things in that table are worth more than the ratios.

**`cascading-errors` at level 2 is 0.98.** The view it reads is 872 bytes
against 47,997 — a 98% cut — and the session costs 2% less. Everything else the
agent spends is reading the file, editing it and building again, and no filter
touches any of that. A compression number is an upper bound on a saving that
mostly does not arrive.

**Level 1 on `mid-stream-trap` went from 1.27 to 0.71** when that level stopped
rendering an empty view. It was costing more than not filtering because an agent
shown nothing runs the command again. The same cell on `last-line-trap` is still
1.25: the answer there is the last line of five thousand, a twenty-line head
does not contain it, and the agent re-runs. Both numbers are the same mechanism
seen twice.

Level 0 costs more than showing the output on three tasks of four, as it has in
every curve so far.

### The other filter as an arm

`--variants rtk` runs the same cells through the filter this one is compared
against: same task, same prompt, same verification, and only the tool in front
of the command differs. `bench/tasks/cargo-cascade` exists for it — the other
tasks run `sh`, `python3` and `rustc`, none of which that tool offers to touch,
so an arm there would measure its coverage rather than its quality. `cargo
build` is on both lists.

On that task, three runs each through cursor:

| arm | bytes it reads | median tokens | against raw |
|---|---|---|---|
| raw | 49,556 | 76,291 | 1.00 |
| the other filter | 49,285 | 76,230 | 1.00 |
| level 2 | 1,453 | 93,102 | 1.22 |

A 97% byte reduction that costs 22% more, against a 0.5% reduction that costs
nothing. The level 2 cells are bimodal — 76k, 93k, 96k — so what the number says
is that this agent sometimes does more work when handed the smaller view, not
that it always does.

The same two cells through the other agent came back 77,329 and 77,531, with
three tool calls and four turns each. So the extra work is one agent's habit
rather than a property of the view, which is the same lesson every curve here
has taught, and the reason both are committed.

### The trap the suite was missing

`last-line-trap` puts the critical line last, where any harness that truncates
long command output keeps it by accident — its raw control passes every run, so
the cell cannot tell a filter from a truncation. `mid-stream-trap` puts the line
at 2,400 of 4,803, 78,746 bytes in and 80,117 bytes from the end, where neither
a head nor a tail reaches it. The token it asks for is derived from a random
blob at print time and recomputed by the verifier, so it cannot be grepped out
of the tree instead of read.

Level 2 keeps that line in 752 bytes out of 158,977 and costs 0.74x. Levels 1
and 0 lose it and cost 1.27x and 1.29x — the ordering the design predicts, and
the reason the task earns its place: it is the only cell where keeping the line
required having ranked it.

Success stayed at 100% for all of them, including the levels that dropped the
answer, because an agent that is not shown a line runs the command again. That
is the general case on this suite: a retention failure surfaces as tokens spent
recovering rather than as a task failed, so the cost column is where to look for
it, and the knee only moves when recovery stops working.

### Comparing against another filter

`bench/image/` builds one image holding both this filter and the one it is being
compared against, and nothing else that differs: the base pinned by digest, the
other tool by release version and checksum, Lens built static from the working
tree. A filter that special-cases commands is measured against those commands,
so a comparison is only about the filters if both read the same git, the same
python and the same rustc. Run the image under a microVM sandbox and every cell
starts from the same machine.

The first thing it measured is that the two tools barely overlap. On
`sh ./migrate.sh` the other one passes 158,976 of 158,977 bytes through
untouched, having no filter for `sh`, where level 2 renders 714. On `git status`
it removes 56% where level 2 removes 1%, and on `git log --stat` 89% against
50%. Neither of those is a result about which tool is better; both are results
about which commands each one knows.

So a comparison suite has to be built from the commands, not from either tool's
design, and the byte column is the beginning of the question rather than the
answer to it — a quarter of a session is what 99.5% of bytes was worth here.

### Running a sweep in a microVM

`--isolation vm` gives every cell its own microVM, booted from the image above,
instead of a temp directory on whatever machine is running the sweep. The agent
runs inside it, so both filters and the commands they filter meet the same
userland every time.

```
LENS_SANDBOX=<cli> LENS_SANDBOX_KERNEL=<vmlinux> CURSOR_API_KEY=<key> \
  bench/runner.py --run --driver cursor --isolation vm
```

Two things about the shape of it. The API key is bound to the one host that
needs it and never enters the guest: the guest gets a placeholder and the
sandbox's broker substitutes the real value on the way out. And the verifier
stays on the host — the guest returns its working directory and `verify.sh`
runs against that copy, because mounting the verifier would put the expected
answer on the same filesystem as the agent being asked to find it.

The preflight boots the image once before spending anything. A sandbox that
cannot find its VMM, an image without the agent in it, and a missing key all
fail the same way at run time — as an agent that never started, which reads
like a rate limit. That misdiagnosis has cost this harness a curve before.

Networking needs `CAP_NET_ADMIN` on the sandbox CLI, so an ungranted binary
fails every cell with a tap-device error. It is recorded as a cell the agent
never attempted, which keeps it out of the success rate.

### Two benchmarks that are not the retention curve

`bench/compare.py` measures compression with no model in the loop. Compression
is deterministic, so it costs nothing: run the command raw, run it under each
filter, count the bytes — every case in `bench/cases/` inside one microVM, so
the git, cargo, python and node they read are the same build.

The cases are the overlap set: commands both tools name a filter for, taken from
their own documentation, declared in `case.toml` **before** anything was
measured. A tool is only run against a command it claims, because reporting 0%
for a command a tool never offered to filter reads as a loss rather than as
coverage. Each tool also gets the invocation its own documentation gives — they
are not the same string, and `rtk grep "pub fn" src/` without `-r` hands grep a
directory, fails, and returns 36 bytes that a byte table scores as a 99% win.

Every case carries a needle: the line the command was run for. What the suite
says, on the twelve cases that measure anything:

| case | raw | level 2 | the other filter |
|---|---|---|---|
| ruff | 53,214 | 38,224 (28%) | 3,686 (94%, 17 of 50 files) |
| cargo clippy | 70,660 | 2,541 (96%) | 235 (100%, needle lost) |
| cargo build | 47,997 | 872 (98%) | 47,771 (1%) |
| grep | 18,541 | 18,541 (0%) | 12,230 (35%) |
| eslint | 12,324 | 12,293 (1%) | 1,036 (92%, 10 of 30 files) |
| git diff | 9,151 | 9,151 (0%) | 6,068 (34%) |
| tsc | 5,049 | 5,049 (0%) | 5,178 (-2%) |
| git log | 4,947 | 3,263 (35%) | 410 (92%) |
| cargo test | 3,651 | 3,391 (8%) | 1,140 (69%) |
| pytest | 2,071 | 2,071 (0%) | 813 (61%) |
| ls | 1,657 | 1,655 (1%) | 158 (91%) |
| git status | 1,155 | 1,154 (1%) | 516 (56%) |
| find | 533 | 533 (0%) | 281 (48%) |

**On the overlap set this filter still loses.** Nine of thirteen cases go the
other way, several by an order of magnitude, and the reason is structural: fifty
ruff findings or a `git log` are not repetition and are not progress. Every line
is distinct and every line is real, and dedupe, progress elision and ranking
have no opinion about them where a per-command parser groups by rule and counts.

Four cases moved when [`cause`](src/pipeline/cause.rs) landed, and two of them
crossed over. `cargo build` went from 1% to 98% — 150 diagnostics reporting one
wrong field type, which is the case neither tool handled — and eslint from 1% to
95%. `cargo clippy` is 2,541 bytes against 235: ten times larger, and the
difference is the lint identifier, which the smaller view spends. It renders one
group as `_`, drops `clippy::needless_return`, and turns 87 of every 90
locations into "+87 more".

Both shapes are recognized now — `ruff`'s `-->` continuation, `pytest`'s `E`
lines — and neither can be grouped: fifty files with one fault each are fifty
places a reader has to visit, and a view naming one of them has lost the other
forty-nine.

What can go is the source each finding quotes. [`excerpt`](src/pipeline/excerpt.rs)
removes it once the view holds more than a handful of findings, because that
excerpt is the one part of a finding the reader already has — it is in the file,
at the line the finding just named. The message stays, the location stays, the
help that says what to do stays. It takes ruff from 1% to 28% with all fifty
files still named, and leaves the rustc cascade alone: grouping already reduced
that to three findings, and three findings keep the source that makes them
actionable.

`bench/compare.py` reports what that costs, per view: the distinct files or
tests a view still names, against the raw output. It is the column that catches
what a needle cannot — a summary that keeps the rule and twenty of the fifty
files it applies to answers the needle and loses the reader thirty places to go.
It caught this filter doing exactly that on eslint, at a 96% reduction that kept
one file of thirty, and the fix is why eslint reads 1% here.

The column measures preservation, not quality. On `cargo test` this filter names
all eighty tests and the other names the six that failed, and the six are the
better view.

`bench/session.py` is the paired-session benchmark, deliberately built to the
same design the other tool publishes so the numbers can be argued about instead
of the method: N pairs of microVMs, one arm filtered and one not, identical
prompt, model and flags, a fixed command sequence so both arms do the same work.
Two differences, both on purpose.

Bytes are counted **per tool result in the transcript**, not from the command.
What the agent's harness delivered is what the model was charged for, and a
harness that truncates a 191KB output before the model sees it has already done
most of the filtering. Measuring the command instead credits the filter with a
saving the control never paid — which is the likeliest way to publish a number
that is real and means nothing.

Sessions are **verified**: a session that ran nine of its twelve commands is
cheaper than one that ran twelve, and cost per session with nothing checking the
work rewards exactly that. Incomplete sessions are dropped from the comparison
rather than averaged into it.

The test is a paired permutation test with a bootstrap interval, not a t-test:
these distributions are small, skewed, and occasionally carry a 2x outlier,
which is the case a t-test handles worst. It has a floor worth knowing — with n
pairs the smallest two-sided p it can produce is 2/2^n, so a five-pair run
cannot come back significant however large the effect. The default is eight.

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

**Level 1 is the failures, or the head of what was kept.** Failures alone is the
right view of a command that failed, and it was the only view this level had: a
command that succeeded rendered as a marker saying nothing was shown, which is
not a smaller view of the output but the absence of one. `grep`, `ls` and
`git diff` all came back that way. When nothing failed it now shows the first
twenty lines the pipeline kept and announces the rest. It is a deliberately
partial view — a search with two hundred and eighty-nine matches does not fit,
and the marker says so — but it is never empty and never silent.

**Parsing carries a flag rather than rediscovering it.** Whether a block has an
indented continuation is tracked as the block is built. Asking by scanning the
block made parsing quadratic in block length, and a command that prints ten
thousand unindented lines is one block: 40k lines took 451ms before the fix and
2ms after.

**A lens flag after the command name reaches the child.** `lens mytool --budget
3` is a valid command line for `mytool`, and Lens does not reinterpret a command
it was asked to run. An unknown flag *before* the command is an error rather
than something to execute.

**One cause is reported once.** A wrong field type is reported at every use
site: 150 diagnostics, one edit, one sentence repeated. `dedupe` cannot see it —
those blocks name different lines, quote different source, and carry caret runs
whose length follows the literal underneath — so `cause` keys on the message
alone and elides every report after the first. Two unrelated errors that open
with the same sentence are grouped too, and the second is announced rather than
shown; the alternative was handing the reader 150 copies, and the store still
has all of them. It runs after `classify`, which is what says a block is a
diagnostic, and before `context`, which force-keeps errors and would otherwise
leave nothing droppable. `context` gained one exception for it: a block grouped
away as another report of a cause already in the view is not that error's
context, and rescuing it is how a cascade returns one neighbour at a time.

**A benchmark credential is brokered, never mounted.** Under `--isolation vm`
the guest is given a placeholder and the sandbox substitutes the real value only
on requests to the one host it is bound to. Mounting a credentials file instead
would work, and was rejected: it puts a live session inside the VM the agent
controls, so the isolation still protects the host and no longer protects the
credential from the thing under test. It also does not avoid the capability the
sandbox needs for networking, and a rotated refresh token can invalidate the
copy left on the host — a sweep that logs you out of your own machine halfway
through. The cost of the broker is that it wants one static credential bound to
one host, which a subscription login cannot provide.

**Interactive detection reads the git subcommand, not just the flag.** `git add
-p` prompts per hunk; `git log -p` is output. Bare `python` is a session;
`python script.py` is a batch job. Wrong in the permissive direction hangs the
user's terminal, so a doubtful case passes through.

## Testing

Tests never touch real cache, config or log directories — isolate through
`LENS_STORE`, `LENS_LOG_DIR` and `LENS_CONFIG` pointed at a temp dir.

The property tests assert the invariants above. Those are the checks that make
the tool's central claim true rather than merely plausible, and they gate CI.
