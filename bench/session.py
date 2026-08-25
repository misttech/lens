#!/usr/bin/env python3
# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""The paired-session benchmark: filter on against filter off, one pair per VM pair.

The retention benchmark asks whether a view is enough to finish a task. This
asks the other question, the one a published comparison argues about: over a
whole agent session, does filtering reduce what the model is charged for?

The design is deliberately the same as the one the other tool publishes, so the
numbers can be argued about rather than the method. N pairs of microVMs, one arm
with the filter on and one without, identical prompt, model and flags in both.
The filtered arm additionally gets the instruction to prefix commands, which is
part of the thing being measured rather than an unfairness.

Two differences, both deliberate:

- **Bytes are counted from the transcript**, per tool result, not from the
  command. What the agent's harness delivered is what the model was charged
  for, and a harness that truncates a 191KB output before the model sees it has
  already done most of the filtering. Measuring the command instead credits the
  filter with a saving the control never paid.
- **Sessions are verified.** A session that skipped half its commands is
  cheaper and worthless, and cost per session with nothing checking the work
  rewards exactly that.

    bench/session.py                    print the plan
    bench/session.py --run --pairs 10

Needs the image from bench/image/build.py and a key for the driving agent.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCH_DIR.parent
RESULTS_DIR = BENCH_DIR / "results"

SANDBOX = os.environ.get("LENS_SANDBOX", "airlock")
SANDBOX_ARGS = ["--local"]
SANDBOX_IMAGE = os.environ.get("LENS_SANDBOX_IMAGE", "lens-bench")
SANDBOX_KERNEL = os.environ.get("LENS_SANDBOX_KERNEL", "")
GUEST_MEM_MIB = 4096
BOOT_GRACE_S = 90

UPSTREAM = ("ANTHROPIC_API_KEY", "api.anthropic.com:x-api-key")

TRANSCRIPT_MARK = "===lens-bench:transcript==="

# The commands the session is told to run, in order. A fixed sequence rather
# than an open problem: the arms have to do the same work for their costs to be
# comparable, and an agent that solves it differently in each arm is measuring
# its own creativity.
AUDIT_COMMANDS = [
    "git status",
    "git log --stat -8",
    "git diff HEAD~3",
    "grep -rn 'pub fn' src/",
    "grep -rn 'TODO\\|FIXME' src/ tests/",
    "find . -name '*.rs' -not -path './out/*'",
    "ls -la src/ src/pipeline/ src/adapters/",
    "wc -l src/*.rs src/pipeline/*.rs",
    "rustc --edition 2021 -o /dev/null session.rs",
    "sh ./deploy-check.sh",
    "sh ./migrate.sh",
    "python3 suite.py",
]

PROMPT = """This repository needs a maintenance audit.

Run every command in the list below with the Bash tool, in order, one per call,
and then write a short report to audit.md covering what the output showed: the
state of the tree, anything failing, and anything that looks like it needs
attention.

{commands}
"""

SYSTEM_ON = (
    "MANDATORY: run all {n} commands using the Bash tool, in order, one per "
    "call. Prefix every one of them with `lens`, e.g. `lens git status`. "
    "`lens` shows you the part of the output worth reading."
)

SYSTEM_OFF = (
    "MANDATORY: run all {n} commands using the Bash tool, in order, one per call."
)


@dataclass
class Session:
    """One agent session: what it cost, and whether it did the work."""

    arm: str
    pair: int
    bash_result_bytes: int = 0
    all_tool_result_bytes: int = 0
    total_input_tokens: int = 0
    cache_read_tokens: int = 0
    cache_creation_tokens: int = 0
    output_tokens: int = 0
    cost_usd: float = 0.0
    api_calls: int = 0
    duration_ms: int = 0
    commands_run: int = 0
    wrote_report: bool = False
    error: str = ""

    @property
    def complete(self) -> bool:
        """Did the session do the work it was paid for?

        A session that ran nine of twelve commands is cheaper than one that ran
        twelve, and comparing their costs says nothing.
        """
        return not self.error and self.commands_run >= len(AUDIT_COMMANDS)


# The chain the report walks, in the order the causes run: the filter changes
# the bytes, the bytes may change the agent's behaviour, behaviour changes the
# tokens, and tokens are what the bill is made of.
CHAIN = [
    ("bash_result_bytes", "bytes of Bash output the model received"),
    ("api_calls", "turns"),
    ("total_input_tokens", "input tokens, cached and not"),
    ("output_tokens", "output tokens"),
    ("cost_usd", "cost in USD"),
    ("all_tool_result_bytes", "bytes from every tool"),
    ("duration_ms", "wall clock in the API"),
]


def stage(pair: int) -> Path:
    """A corpus for one session: this repository, plus the task fixtures.

    Cloned rather than mounted so the session can write to it, and rebuilt per
    session so one arm cannot inherit the other's edits.
    """
    work = Path(tempfile.mkdtemp(prefix=f"lens-session-{pair}-"))
    subprocess.run(
        ["git", "clone", "--quiet", str(REPO_ROOT), str(work)],
        check=True,
        capture_output=True,
    )
    for name in ("cascading-errors", "last-line-trap", "mid-stream-trap"):
        setup = BENCH_DIR / "tasks" / name / "setup.sh"
        if setup.is_file():
            subprocess.run(
                ["sh", str(setup), str(work)], check=True, capture_output=True
            )
    fixture = BENCH_DIR / "tasks" / "fix-failing-test" / "setup.sh"
    if fixture.is_file():
        subprocess.run(["sh", str(fixture), str(work)], check=True, capture_output=True)
    return work


def guest_script(arm: str, model: str, max_turns: int) -> str:
    """What one session runs inside its microVM."""
    commands = "\n".join(f"    {command}" for command in AUDIT_COMMANDS)
    prompt = PROMPT.format(commands=commands)
    system = (SYSTEM_ON if arm == "on" else SYSTEM_OFF).format(n=len(AUDIT_COMMANDS))

    # The filtered arm is told how to use the filter, in the file the agent reads
    # by convention. That instruction is part of the tool, not a thumb on the
    # scale — an unused filter is not a filter.
    claude_md = (
        f"echo {shell_quote(system)} > /tmp/work/CLAUDE.md\n" if arm == "on" else ""
    )

    return f"""set -u
mkdir -p /tmp/work "$HOME"
cp -a /mnt/task/. /tmp/work/
cd /tmp/work
git config --global --add safe.directory '*' 2>/dev/null || true
{claude_md}claude -p {shell_quote(prompt)} \\
  --output-format stream-json --verbose \\
  --model {shell_quote(model)} --max-turns {max_turns} \\
  --dangerously-skip-permissions \\
  --append-system-prompt {shell_quote(system)} \\
  > /tmp/transcript.jsonl 2>/tmp/agent.err || true
echo '{TRANSCRIPT_MARK}'
cat /tmp/transcript.jsonl
"""


def shell_quote(text: str) -> str:
    return "'" + text.replace("'", "'\\''") + "'"


def parse_transcript(text: str, session: Session) -> None:
    """Pull the metrics out of what the agent's harness wrote.

    Bytes are counted per tool result and matched back to the call that
    produced them, because that is the number the model was actually charged
    for. A command that printed 191KB into a harness that truncated it to 30KB
    cost 30KB, and a filter is only owed credit for the difference it made to
    that.
    """
    tools: dict[str, str] = {}
    ran: list[str] = []

    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue

        content = event.get("message", {}).get("content", [])
        if isinstance(content, list):
            for part in content:
                if not isinstance(part, dict):
                    continue
                if part.get("type") == "tool_use":
                    tools[part.get("id", "")] = part.get("name", "")
                    if part.get("name") == "Bash":
                        ran.append(str(part.get("input", {}).get("command", "")))
                elif part.get("type") == "tool_result":
                    body = part.get("content", "")
                    if isinstance(body, list):
                        body = "".join(
                            block.get("text", "")
                            for block in body
                            if isinstance(block, dict)
                        )
                    size = len(str(body).encode("utf-8"))
                    session.all_tool_result_bytes += size
                    if tools.get(part.get("tool_use_id", "")) == "Bash":
                        session.bash_result_bytes += size

        if event.get("type") == "result":
            usage = event.get("usage", {}) or {}
            session.cache_read_tokens = int(
                usage.get("cache_read_input_tokens", 0) or 0
            )
            session.cache_creation_tokens = int(
                usage.get("cache_creation_input_tokens", 0) or 0
            )
            session.output_tokens = int(usage.get("output_tokens", 0) or 0)
            session.total_input_tokens = (
                int(usage.get("input_tokens", 0) or 0)
                + session.cache_read_tokens
                + session.cache_creation_tokens
            )
            session.cost_usd = float(event.get("total_cost_usd", 0.0) or 0.0)
            session.api_calls = int(event.get("num_turns", 0) or 0)
            session.duration_ms = int(event.get("duration_api_ms", 0) or 0)

    session.commands_run = count_run(ran)


def count_run(calls: list[str]) -> int:
    """How many mandated commands were actually run.

    Matched against the first line of the call, with any filter prefix stripped,
    rather than anywhere in its text. A command that writes a script containing
    these command lines — a heredoc, a generated runner — otherwise counts as
    having run every one of them, which was measured happening on a real
    transcript that had never been given this task.
    """
    issued = set()
    for call in calls:
        first = call.strip().splitlines()[0].strip() if call.strip() else ""
        for prefix in ("lens ", "rtk "):
            if first.startswith(prefix):
                first = first[len(prefix) :].lstrip()
        issued.add(first)

    return sum(
        1
        for command in AUDIT_COMMANDS
        if any(line == command or line.startswith(command) for line in issued)
    )


def run_session(
    arm: str, pair: int, model: str, max_turns: int, image: str, kernel: str
) -> Session:
    """One arm of one pair, in its own microVM."""
    session = Session(arm=arm, pair=pair)
    work = stage(pair)
    try:
        name, upstream = UPSTREAM
        command = [
            SANDBOX,
            *SANDBOX_ARGS,
            "run",
            image,
            "--mem",
            str(GUEST_MEM_MIB),
            "--mount-dir",
            f"{work}:/mnt/task",
            "--allow-dns",
            "--secret",
            f"{name}={upstream}",
        ]
        if kernel:
            command += ["--kernel", kernel]
        command += ["--", "sh", "-c", guest_script(arm, model, max_turns)]

        try:
            proc = subprocess.run(
                command,
                capture_output=True,
                text=True,
                timeout=SESSION_TIMEOUT_S + BOOT_GRACE_S,
                check=False,
            )
        except subprocess.TimeoutExpired:
            session.error = "timeout"
            return session

        if TRANSCRIPT_MARK not in proc.stdout:
            stderr = [line for line in proc.stderr.splitlines() if line.strip()]
            session.error = (
                f"sandbox: {stderr[-1][:160] if stderr else 'no transcript'}"
            )
            return session

        parse_transcript(proc.stdout.split(TRANSCRIPT_MARK, 1)[1], session)
        if session.total_input_tokens == 0:
            # No completed API call: an auth failure or an exhausted key, not a
            # session that happened to be cheap. Counting it would drag the arm
            # it landed in towards zero.
            session.error = "no API call completed"
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return session


SESSION_TIMEOUT_S = 900


def paired(sessions: list[Session]) -> list[tuple[Session, Session]]:
    """Pairs where both arms did the work. Everything else is dropped."""
    by_pair: dict[int, dict[str, Session]] = {}
    for session in sessions:
        by_pair.setdefault(session.pair, {})[session.arm] = session
    pairs = []
    for arms in by_pair.values():
        on, off = arms.get("on"), arms.get("off")
        if on and off and on.complete and off.complete:
            pairs.append((on, off))
    return pairs


def permutation_p(diffs: list[float], trials: int = 20000) -> float:
    """Paired permutation test: how often would sign flips produce this?

    Not a t-test. The distributions here are small, skewed and occasionally
    carry a 2x outlier, which is the case a t-test handles worst and this one
    does not have to assume anything about.
    """
    if len(diffs) < 2:
        return 1.0
    observed = abs(statistics.fmean(diffs))
    rng = random.Random(0)
    hits = 0
    for _ in range(trials):
        flipped = [d if rng.random() < 0.5 else -d for d in diffs]
        if abs(statistics.fmean(flipped)) >= observed:
            hits += 1
    return (hits + 1) / (trials + 1)


def bootstrap_ci(diffs: list[float], trials: int = 10000) -> tuple[float, float]:
    """Percentile interval on the paired difference, resampling pairs."""
    if len(diffs) < 2:
        return (0.0, 0.0)
    rng = random.Random(1)
    means = sorted(
        statistics.fmean(rng.choices(diffs, k=len(diffs))) for _ in range(trials)
    )
    lo = means[int(0.025 * trials)]
    hi = means[int(0.975 * trials)]
    return (lo, hi)


def report(sessions: list[Session]) -> dict:
    """The chain, in the order the causes run."""
    pairs = paired(sessions)
    dropped = len(sessions) - 2 * len(pairs)
    print(f"\n{len(pairs)} usable pair(s); {dropped} session(s) dropped")
    for session in sessions:
        if session.error:
            print(f"  pair {session.pair} {session.arm}: {session.error}")
        elif not session.complete:
            print(
                f"  pair {session.pair} {session.arm}: ran "
                f"{session.commands_run}/{len(AUDIT_COMMANDS)} commands"
            )

    if not pairs:
        print("\nnothing to compare")
        return {"pairs": 0, "metrics": {}}

    print(
        f"\n{'metric':<26}{'filtered':>12}{'control':>12}{'saving':>9}"
        f"{'p':>8}   95% CI of the difference"
    )
    metrics = {}
    for key, label in CHAIN:
        on = [float(getattr(a, key)) for a, _ in pairs]
        off = [float(getattr(b, key)) for _, b in pairs]
        diffs = [b - a for a, b in zip(on, off, strict=True)]
        on_mean, off_mean = statistics.fmean(on), statistics.fmean(off)
        saving = (1 - on_mean / off_mean) * 100 if off_mean else 0.0
        p = permutation_p(diffs)
        lo, hi = bootstrap_ci(diffs)
        metrics[key] = {
            "on_mean": on_mean,
            "off_mean": off_mean,
            "saving_pct": round(saving, 1),
            "p": round(p, 4),
            "ci": [round(lo, 2), round(hi, 2)],
        }
        star = "*" if p < 0.05 else " "
        print(
            f"{label:<26}{on_mean:>12,.0f}{off_mean:>12,.0f}"
            f"{saving:>8.1f}%{p:>8.3f}{star}  [{lo:,.0f}, {hi:,.0f}]"
        )

    print("\n* p < 0.05, paired permutation test; CI is a percentile bootstrap")
    print("a saving is positive when the filtered arm used less")
    return {"pairs": len(pairs), "metrics": metrics}


def preflight(image: str, kernel: str) -> str:
    """Everything a paid run needs, checked before it spends anything."""
    if shutil.which(SANDBOX) is None:
        raise SystemExit(f"no {SANDBOX} on PATH — set LENS_SANDBOX to the CLI")
    if not os.environ.get(UPSTREAM[0]):
        raise SystemExit(
            f"{UPSTREAM[0]} is not set — the guest is given it by the broker"
        )
    if not kernel:
        raise SystemExit("no guest kernel — set LENS_SANDBOX_KERNEL or pass --kernel")

    inspect = subprocess.run(
        [SANDBOX, *SANDBOX_ARGS, "image", "inspect", image],
        capture_output=True,
        text=True,
        check=False,
    )
    if inspect.returncode != 0:
        raise SystemExit(
            f"image {image} is not loaded — build it with bench/image/build.py"
        )

    boot = subprocess.run(
        [
            SANDBOX,
            *SANDBOX_ARGS,
            "run",
            image,
            "--kernel",
            kernel,
            "--mem",
            str(GUEST_MEM_MIB),
            "--",
            "sh",
            "-c",
            "command -v lens >/dev/null || exit 3\n"
            "command -v claude >/dev/null || exit 4",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if boot.returncode == 3:
        raise SystemExit(f"image {image} has no lens on PATH")
    if boot.returncode == 4:
        raise SystemExit(f"image {image} has no claude — rebuild it with the agent")
    if boot.returncode != 0:
        stderr = [line for line in boot.stderr.splitlines() if line.strip()]
        raise SystemExit(f"image {image} does not boot: {stderr[-1] if stderr else ''}")
    return json.loads(inspect.stdout).get("digest", "")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--run", action="store_true", help="actually run; costs credits"
    )
    # Six is the floor at which this test can return a significant result at
    # all: with n pairs the smallest two-sided p a sign-flip test can produce is
    # 2/2^n, which is 0.063 at five pairs and 0.033 at six. A five-pair run
    # cannot come back significant however large the effect, and paying for one
    # to discover that is the kind of mistake this file exists to prevent.
    parser.add_argument("--pairs", type=int, default=8)
    parser.add_argument("--model", default="claude-opus-4-7")
    parser.add_argument("--max-turns", type=int, default=25)
    parser.add_argument("--image", default=SANDBOX_IMAGE)
    parser.add_argument("--kernel", default=SANDBOX_KERNEL)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    floor = 2 / (2**args.pairs)
    if args.pairs < 6:
        print(
            f"warning: {args.pairs} pairs cannot produce p < 0.05 "
            f"(best possible is {floor:.3f}); use at least 6",
            file=sys.stderr,
        )

    if not args.run:
        print(f"pairs     {args.pairs}  ({2 * args.pairs} sessions, one VM each)")
        print(f"best p    {floor:.3f}  the smallest this many pairs can produce")
        print(f"model     {args.model}")
        print(f"commands  {len(AUDIT_COMMANDS)} per session, in order")
        print("arms      on: prefix with lens · off: bare")
        print("\nEach session is one microVM. This spends API credits; add --run.")
        return 0

    digest = preflight(args.image, args.kernel)
    print(f"image     {args.image} {digest[:19]}")

    sessions: list[Session] = []
    for pair in range(args.pairs):
        for arm in ("off", "on"):
            started = time.monotonic()
            print(f"[pair {pair + 1}/{args.pairs}] {arm}", flush=True)
            session = run_session(
                arm, pair, args.model, args.max_turns, args.image, args.kernel
            )
            sessions.append(session)
            print(
                f"          {session.commands_run}/{len(AUDIT_COMMANDS)} commands  "
                f"{session.bash_result_bytes:,} bash bytes  "
                f"{session.total_input_tokens:,} in  ${session.cost_usd:.4f}  "
                f"{time.monotonic() - started:.0f}s"
                + (f"  {session.error}" if session.error else ""),
                flush=True,
            )

    summary = report(sessions)
    if args.out:
        payload = {
            "image": digest,
            "model": args.model,
            "commands": AUDIT_COMMANDS,
            "summary": summary,
            "sessions": [vars(s) for s in sessions],
        }
        args.out.write_text(json.dumps(payload, indent=2) + "\n")
        print(f"\nwrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
