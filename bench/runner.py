#!/usr/bin/env python3
# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""The retention benchmark.

Compression is easy to measure and easy to fake. The question this answers is
the one that decides whether Lens is worth using: with less output, does the
agent still finish the job?

For every (task, variant, repeat) it starts from a clean working directory,
runs a real agent against a real command, and asks a script whether the work was
actually done. Success is mechanically verifiable — a suite that passes, a
program that compiles, a file that contains the right string. No model judges
another model's work, because that would put the one number that has to be
trustworthy back into the hands of the thing being measured.

The headline is not the compression ratio. It is the knee: the point at which
task success starts to fall. A run that reports 90% fewer tokens and 60% success
is a worse tool than one reporting 70% fewer and 100%.

    bench/runner.py                 print the plan; spend nothing
    bench/runner.py --run           run it; this costs API credits
    bench/runner.py --run --tasks last-line-trap --variants raw,level2
    bench/runner.py --run --save-baseline
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import io
import json
import os
import shlex
import shutil
import statistics
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCH_DIR.parent
TASKS_DIR = BENCH_DIR / "tasks"
RESULTS_DIR = BENCH_DIR / "results"
# One committed curve per driver. Saving a cursor run into the claude file
# would publish one agent's numbers under another agent's name, and nothing in
# the file would contradict it — the driver is recorded inside, where a reader
# who already trusts the filename will not look.
BASELINES = {
    "claude": RESULTS_DIR / "retention-baseline.json",
    "cursor": RESULTS_DIR / "retention-cursor.json",
}

# How the command's output reaches the agent.
#
# Every variant runs the same command line. The agent is always told to type
# `lens <cmd>`, and what differs is only what Lens then shows it — `raw` is a
# passthrough, byte-identical to running the command directly.
#
# This matters more than it looks. The first version of this file substituted an
# absolute path into the prompt for filtered variants and nothing for the
# control, so the two arms differed in the instruction as well as the output:
# the agent skipped the long unfamiliar path, ran the command its own way, and
# the numbers compared two different behaviours rather than two views.
#
# Levels are the sweep this build can offer. A token budget is a knob the
# ranking stages have not grown yet, and inventing one here would measure
# nothing.
# Each variant names the filter it types and the environment it types it in.
# The other filter is a variant rather than a second benchmark: same task, same
# prompt, same verification, and only the tool in front of the command differs.
VARIANTS: dict[str, dict] = {
    "raw": {"tool": "lens", "env": {"LENS_MODE": "raw"}},
    "level3": {"tool": "lens", "env": {"LENS_LEVEL": "3"}},
    "level2": {"tool": "lens", "env": {"LENS_LEVEL": "2"}},
    "level1": {"tool": "lens", "env": {"LENS_LEVEL": "1"}},
    "level0": {"tool": "lens", "env": {"LENS_LEVEL": "0"}},
    "rtk": {"tool": "rtk", "env": {}},
}

DEFAULT_VARIANTS = ["raw", "level2", "level1", "level0"]

# The microVM sandbox, named by env rather than hard-coded: the harness needs a
# CLI that can boot an image, mount a directory read-only, and bind a secret to
# one upstream — not one particular implementation of those verbs.
SANDBOX = os.environ.get("LENS_SANDBOX", "airlock")
SANDBOX_ARGS = ["--local"]
SANDBOX_IMAGE = os.environ.get("LENS_SANDBOX_IMAGE", "lens-bench")
SANDBOX_KERNEL = os.environ.get("LENS_SANDBOX_KERNEL", "")
GUEST_MEM_MIB = 2048

# Boot, image mount and the copy back, on top of the task's own timeout.
BOOT_GRACE_S = 60

# What each agent has to reach, and under which header. The key is bound to
# that host: the guest is given a placeholder and the real value is substituted
# on the way out, so it never enters the VM. Putting a live key inside the
# sandbox would hand it to the thing under test.
UPSTREAM = {
    "cursor": ("CURSOR_API_KEY", "api2.cursor.sh"),
    "claude": ("ANTHROPIC_API_KEY", "api.anthropic.com:x-api-key"),
}

# A cell's stdout carries the agent's output and then the work directory. Two
# markers, because the sandbox gives one stdout and the verifier needs both.
AGENT_MARK = "===lens-bench:agent==="
WORK_MARK = "===lens-bench:work==="


@dataclass
class Task:
    """One benchmark task, loaded from its directory."""

    name: str
    summary: str
    prompt: str
    turn_limit: int
    timeout_s: int
    directory: Path

    @property
    def setup(self) -> Path:
        return self.directory / "setup.sh"

    @property
    def verify(self) -> Path:
        return self.directory / "verify.sh"


# How long to wait before retrying a cell the agent never attempted.
RETRY_PAUSE_S = 20


def attempted(result: dict) -> bool:
    """Did the agent actually try, and get to finish?

    Zero tokens means it never started. A non-zero exit means it started and
    was cut off — a credential, a limit, a crash — and the tokens it spent
    before that say nothing about whether the view was enough to do the work.
    Both are cells the sweep did not get, and neither is the filter losing.
    """
    if result.get("harness_error"):
        return False
    return total_model_tokens(result.get("usage", {}) or {}) > 0


@dataclass
class Cell:
    """One (task, variant, repeat) result."""

    task: str
    variant: str
    repeat: int
    passed: bool
    turns: int = 0
    tool_calls: int = 0
    model_tokens: int = 0
    output_tokens: int = 0
    cost_usd: float = 0.0
    wall_s: float = 0.0
    note: str = ""


@dataclass
class Summary:
    """Every cell for one (task, variant), reduced to what the curve needs."""

    task: str
    variant: str
    runs: int
    passed: int
    model_tokens: list[int] = field(default_factory=list)
    tool_calls: list[int] = field(default_factory=list)
    turns: list[int] = field(default_factory=list)
    cost_usd: float = 0.0

    @property
    def success_rate(self) -> float:
        return self.passed / self.runs if self.runs else 0.0


def load_tasks(only: list[str] | None) -> list[Task]:
    """Read every task directory, or just the named ones."""
    tasks = []
    for path in sorted(TASKS_DIR.iterdir()):
        if not (path / "task.toml").is_file():
            continue
        if only and path.name not in only:
            continue
        spec = tomllib.loads((path / "task.toml").read_text())
        tasks.append(
            Task(
                name=spec["name"],
                summary=spec.get("summary", ""),
                prompt=spec["prompt"].strip(),
                turn_limit=int(spec.get("turn_limit", 20)),
                timeout_s=int(spec.get("timeout_s", 300)),
                directory=path,
            )
        )
    return tasks


def binary_fingerprint() -> str:
    """Which build produced a curve.

    Rebuilding mid-sweep silently splits a run across two versions of the thing
    being measured — early cells describe one filter and later cells another.
    Recording the fingerprint does not prevent that, but it means a curve can be
    checked against the build it claims to describe.
    """
    binary = lens_binary()
    if not binary.is_file():
        return "missing"
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()[:12]
    return f"{digest} ({binary.stat().st_size} bytes)"


def filter_binary(tool: str) -> Path:
    """Where a variant's filter lives.

    The other filter is not built here, so it is found rather than located: an
    installed one, or the path in LENS_RTK.
    """
    if tool == "lens":
        return lens_binary()

    override = os.environ.get("LENS_RTK")
    if override:
        return Path(override)
    found = shutil.which(tool)
    if found is None:
        raise SystemExit(f"no {tool} on PATH — set LENS_RTK to the binary")
    return Path(found)


def lens_binary() -> Path:
    """The binary under test, built by `make build`."""
    target = os.uname().sysname.lower()
    machine = os.uname().machine
    arch = {"x86_64": "amd64", "aarch64": "arm64", "arm64": "arm64"}.get(
        machine, machine
    )
    return REPO_ROOT / "out" / target / arch / "lens"


# The agents this can drive. Cross-running matters: a curve produced by one
# agent measures that agent's habits as much as the filter's quality, and a
# result that only holds for one of them is a result about the agent.
#
# Taken from the baseline map so a driver cannot exist without a file to record
# it in. That mistake would otherwise surface at the end of a paid sweep, with
# the results still only in memory.
DRIVERS = tuple(BASELINES)

# What each driver reports. Cursor's single result object carries tokens but
# neither turns nor tool calls, so those read zero for it — recorded as a gap
# rather than filled in with a guess.
#
# The cursor default is a Grok model rather than the strongest one available:
# a cross-run is worth having only if it can actually run, and per-model quota
# is the thing most likely to stop it.
DEFAULT_MODEL = {"claude": "claude-sonnet-5", "cursor": "cursor-grok-4.6-medium"}


def agent_command(driver: str, prompt: str, model: str, turn_limit: int) -> list[str]:
    """The command line for one agent run."""
    if driver == "cursor":
        return [
            "cursor-agent",
            "-p",
            prompt,
            "--output-format",
            "json",
            "--model",
            model,
            # Cursor refuses to touch an untrusted directory, and every work
            # directory here is one this harness just created.
            "--force",
        ]
    return [
        "claude",
        "-p",
        prompt,
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        model,
        "--max-turns",
        str(turn_limit),
        "--permission-mode",
        "bypassPermissions",
    ]


def parse_agent_output(driver: str, stdout: str) -> tuple[dict, int]:
    """Pull the result object and a tool-call count out of what the agent wrote."""
    if driver == "cursor":
        for line in reversed(stdout.splitlines()):
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get("type") == "result":
                return event, 0
        return {"is_error": True, "subtype": "no_result"}, 0

    result: dict = {"is_error": True, "subtype": "no_result"}
    tool_calls = 0
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "result":
            result = event
        content = event.get("message", {}).get("content", [])
        if isinstance(content, list):
            tool_calls += sum(1 for part in content if part.get("type") == "tool_use")
    return result, tool_calls


def run_agent(
    task: Task, work: Path, variant: str, model: str, driver: str
) -> tuple[dict, int, float]:
    """Run the agent once. Returns (result object, tool calls, wall seconds).

    stream-json rather than json: the result object carries turns and tokens but
    not tool calls, and tool calls are what catch the failure mode this whole
    benchmark exists to detect — a filter that saves tokens per call by causing
    more calls has not saved anything.
    """
    # The filter on PATH under its own name, so every variant's prompt is the
    # same shape and the only difference between arms is what the command shows.
    tool = VARIANTS[variant]["tool"]
    bin_dir = work / ".bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    link = bin_dir / tool
    if not link.exists():
        link.symlink_to(filter_binary(tool))

    prompt = task.prompt.replace("{lens}", tool).strip()

    env = dict(os.environ)
    env.update(VARIANTS[variant]["env"])
    env["PATH"] = f"{bin_dir}:{env.get('PATH', '')}"
    # Isolate the store and log: a benchmark must not read or write a
    # developer's real cache.
    env["LENS_STORE"] = str(work / ".lens" / "store")
    env["LENS_LOG_DIR"] = str(work / ".lens" / "logs")

    command = agent_command(driver, prompt, model, task.turn_limit)

    started = time.monotonic()
    try:
        proc = subprocess.run(
            command,
            cwd=work,
            env=env,
            capture_output=True,
            text=True,
            timeout=task.timeout_s,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return {"is_error": True, "subtype": "timeout"}, 0, time.monotonic() - started

    wall = time.monotonic() - started
    result, tool_calls = parse_agent_output(driver, proc.stdout)

    if proc.returncode != 0:
        # A cell that failed for a reason outside the thing being measured has
        # to say so, and be excluded rather than counted. Scoring it as a task
        # failure blames the filter for the harness — measured happening: a
        # sweep cut off partway recorded a cell that had spent 13,493 tokens as
        # a failure, and the curve came back with a knee that was not there.
        last = [line for line in proc.stderr.splitlines() if line.strip()]
        result["harness_error"] = True
        result["subtype"] = (
            f"agent exit {proc.returncode}: {last[-1][:120] if last else ''}"
        )

    return result, tool_calls, wall


def guest_script(task: Task, variant: str, model: str, driver: str) -> str:
    """The one script a cell runs inside its microVM.

    The verifier stays on the host. Copying it in would put the expected answer
    on the same filesystem as the agent being asked to go and find it, and a
    task that can be passed by reading the marking scheme measures nothing.
    """
    assignments = " ".join(
        f"{name}={shlex.quote(value)}"
        for name, value in sorted(VARIANTS[variant]["env"].items())
    )
    prompt = task.prompt.replace("{lens}", VARIANTS[variant]["tool"]).strip()
    agent = " ".join(
        shlex.quote(arg)
        for arg in agent_command(driver, prompt, model, task.turn_limit)
    )
    # The agent's own failure is data, not a reason to abandon the cell: `|| true`
    # so the work directory still comes back and verify still gets to speak.
    return f"""set -eu
mkdir -p /tmp/work "$HOME"
cp -a /mnt/task/. /tmp/work/
cd /tmp/work
{assignments} {agent} >/tmp/agent.out 2>/tmp/agent.err || true
echo '{AGENT_MARK}'
cat /tmp/agent.out
echo '{WORK_MARK}'
tar c -C /tmp/work . | base64 -w0
echo
"""


def split_sections(stdout: str) -> tuple[str, str]:
    """Pull the agent's output and the packed work directory back apart."""
    if AGENT_MARK not in stdout or WORK_MARK not in stdout:
        return "", ""
    _, rest = stdout.split(AGENT_MARK, 1)
    agent, packed = rest.split(WORK_MARK, 1)
    return agent.strip(), packed.strip()


def restore(work: Path, packed: str) -> None:
    """Unpack the guest's work directory over the host's copy.

    Verification then runs against what the agent actually left behind, on the
    host, where the task's own verify.sh already lives.
    """
    raw = base64.b64decode(packed)
    with tarfile.open(fileobj=io.BytesIO(raw)) as tar:
        tar.extractall(work, filter="data")


def run_agent_vm(
    task: Task,
    work: Path,
    variant: str,
    model: str,
    driver: str,
    image: str,
    kernel: str,
) -> tuple[dict, int, float]:
    """Run one cell in its own microVM, and bring the work directory back."""
    name, upstream = UPSTREAM[driver]
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
        "--timeout",
        f"{task.timeout_s}s",
    ]
    if kernel:
        command += ["--kernel", kernel]
    command += ["--", "sh", "-c", guest_script(task, variant, model, driver)]

    started = time.monotonic()
    try:
        proc = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=task.timeout_s + BOOT_GRACE_S,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return {"is_error": True, "subtype": "timeout"}, 0, time.monotonic() - started

    wall = time.monotonic() - started
    agent_out, packed = split_sections(proc.stdout)
    if packed:
        restore(work, packed)

    if not agent_out:
        # The cell never got as far as the agent. That is the sandbox's failure,
        # not the filter's, and it has to be recorded as one.
        stderr = [line for line in proc.stderr.splitlines() if line.strip()]
        detail = stderr[-1][:160] if stderr else f"exit {proc.returncode}"
        return {"is_error": True, "subtype": f"sandbox: {detail}"}, 0, wall

    result, tool_calls = parse_agent_output(driver, agent_out)
    return result, tool_calls, wall


def preflight_vm(image: str, kernel: str, driver: str) -> str:
    """Check what a VM sweep needs before it spends anything. Returns the digest.

    Every one of these failures would otherwise surface in the first cell, or
    worse, in all of them — as an agent that never started, which reads like a
    rate limit rather than a missing flag.
    """
    if shutil.which(SANDBOX) is None:
        raise SystemExit(f"no {SANDBOX} on PATH — set LENS_SANDBOX to the CLI")

    name, _ = UPSTREAM[driver]
    if not os.environ.get(name):
        raise SystemExit(f"{name} is not set — the guest is given it by the broker")

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

    # Boot it once. A sandbox that cannot find its VMM, or an image without the
    # agent in it, fails identically to an agent that never started — and that
    # reads like a rate limit, which is the one misdiagnosis this harness has
    # already been burned by.
    agent = {"cursor": "cursor-agent", "claude": "claude"}[driver]
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
            f"command -v lens >/dev/null || exit 3\n"
            f"command -v {agent} >/dev/null || exit 4",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if boot.returncode == 3:
        raise SystemExit(f"image {image} has no lens on PATH")
    if boot.returncode == 4:
        raise SystemExit(f"image {image} has no {agent} — rebuild it with --agent")
    if boot.returncode != 0:
        stderr = [line for line in boot.stderr.splitlines() if line.strip()]
        raise SystemExit(f"image {image} does not boot: {stderr[-1] if stderr else ''}")

    return json.loads(inspect.stdout).get("digest", "")


def total_model_tokens(usage: dict) -> int:
    """Everything the model was charged for, not just what it wrote.

    Cache reads count. They are cheaper, not free, and a filter that shrinks the
    written output while growing the context is not an improvement.
    """
    keys = (
        # claude
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        # cursor
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
    )
    return sum(int(usage.get(key, 0) or 0) for key in keys)


def run_cell(
    task: Task,
    variant: str,
    repeat: int,
    model: str,
    driver: str,
    isolation: str = "host",
    image: str = SANDBOX_IMAGE,
    kernel: str = SANDBOX_KERNEL,
) -> Cell:
    """Set up, run, verify, tear down.

    An agent that returns in a couple of seconds having spent no tokens did not
    attempt the task — a rate limit, an expired credential, a CLI that could not
    start. Scoring that as a task failure would blame the filter for the
    harness, so it is retried once and then recorded as what it is.
    """
    work = Path(tempfile.mkdtemp(prefix=f"lens-bench-{task.name}-"))
    try:
        subprocess.run(
            ["sh", str(task.setup), str(work)], check=True, capture_output=True
        )

        def once() -> tuple[dict, int, float]:
            if isolation == "vm":
                return run_agent_vm(task, work, variant, model, driver, image, kernel)
            return run_agent(task, work, variant, model, driver)

        result, tool_calls, wall = once()
        if not attempted(result):
            time.sleep(RETRY_PAUSE_S)
            result, tool_calls, wall = once()

        verified = subprocess.run(
            ["sh", str(task.verify), str(work)], capture_output=True, check=False
        )
        usage = result.get("usage", {}) or {}

        return Cell(
            task=task.name,
            variant=variant,
            repeat=repeat,
            # A cell the agent never attempted is neither a pass nor a fail:
            # verify.sh reports on work that was never done, and counting that
            # as the filter losing is how a rate limit becomes a false result.
            passed=attempted(result) and verified.returncode == 0,
            turns=int(result.get("num_turns", 0) or 0),
            tool_calls=tool_calls,
            model_tokens=total_model_tokens(usage),
            output_tokens=int(
                usage.get("output_tokens", usage.get("outputTokens", 0)) or 0
            ),
            cost_usd=float(result.get("total_cost_usd", 0.0) or 0.0),
            wall_s=wall,
            note=cell_note(result),
        )
    finally:
        shutil.rmtree(work, ignore_errors=True)


def cell_note(result: dict) -> str:
    """Why a cell is not a clean result, if it is not."""
    if not attempted(result):
        return f"not attempted: {result.get('subtype', 'unknown')}"
    if result.get("is_error"):
        return str(result.get("subtype", "error"))
    return ""


def summarize(cells: list[Cell]) -> list[Summary]:
    """Group cells into one row per (task, variant)."""
    rows: dict[tuple[str, str], Summary] = {}
    for cell in cells:
        if cell.note.startswith("not attempted"):
            # Excluded from the rate entirely. Including it would move the
            # number the whole benchmark exists to report.
            continue
        key = (cell.task, cell.variant)
        row = rows.get(key)
        if row is None:
            row = Summary(task=cell.task, variant=cell.variant, runs=0, passed=0)
            rows[key] = row
        row.runs += 1
        row.passed += int(cell.passed)
        row.model_tokens.append(cell.model_tokens)
        row.tool_calls.append(cell.tool_calls)
        row.turns.append(cell.turns)
        row.cost_usd += cell.cost_usd
    return list(rows.values())


def median(values: list[int]) -> int:
    return int(statistics.median(values)) if values else 0


def report(summaries: list[Summary]) -> None:
    """Print the curve.

    Success first, because it is the only column that decides anything. Output
    tokens are reported last and are the vanity metric: a tool can win that
    column by deleting the answer.
    """
    print()
    print(
        f"{'task':<20} {'variant':<8} {'success':>9} {'model tok':>10} "
        f"{'tools':>6} {'turns':>6} {'cost':>8}"
    )
    for row in sorted(summaries, key=lambda r: (r.task, r.variant)):
        rate = f"{row.passed}/{row.runs}"
        print(
            f"{row.task:<20} {row.variant:<8} {rate:>9} "
            f"{median(row.model_tokens):>10} {median(row.tool_calls):>6} "
            f"{median(row.turns):>6} {row.cost_usd:>7.2f}$"
        )

    knee = find_knee(summaries)
    print()
    if knee:
        print(f"knee: success falls below 100% at {knee}")
    else:
        print("knee: none — every variant held full success")


def find_knee(summaries: list[Summary]) -> str | None:
    """The first variant, cheapest view last, where success stops being total.

    This is the number worth publishing. "Holds full success at level 1" says
    something no compression ratio can.
    """
    order = [v for v in VARIANTS if any(s.variant == v for s in summaries)]
    for variant in order:
        rows = [s for s in summaries if s.variant == variant]
        if rows and any(s.success_rate < 1.0 for s in rows):
            return variant
    return None


def to_json(
    cells: list[Cell],
    summaries: list[Summary],
    model: str,
    driver: str,
    isolation: str = "host",
    image: str = "",
) -> str:
    return json.dumps(
        {
            "driver": driver,
            "model": model,
            "binary": binary_fingerprint(),
            "isolation": isolation,
            # Empty on the host path: there is no image, and the environment is
            # whatever the machine happened to be.
            "image": image,
            "knee": find_knee(summaries),
            "summaries": [
                {
                    "task": s.task,
                    "variant": s.variant,
                    "runs": s.runs,
                    "passed": s.passed,
                    "success_rate": round(s.success_rate, 3),
                    "median_model_tokens": median(s.model_tokens),
                    "median_tool_calls": median(s.tool_calls),
                    "median_turns": median(s.turns),
                    "cost_usd": round(s.cost_usd, 4),
                }
                for s in sorted(summaries, key=lambda r: (r.task, r.variant))
            ],
            "cells": [vars(c) for c in cells],
        },
        indent=2,
    )


def plan(
    tasks: list[Task],
    variants: list[str],
    repeats: int,
    model: str,
    driver: str,
    isolation: str = "host",
) -> None:
    """Say what a run would do, and what it would cost, without doing it."""
    cells = len(tasks) * len(variants) * repeats
    print(f"driver    {driver}")
    print(f"isolation {isolation}")
    print(f"model     {model}")
    print(f"tasks     {', '.join(t.name for t in tasks)}")
    print(f"variants  {', '.join(variants)}")
    print(f"repeats   {repeats}")
    print(f"cells     {cells}  ({cells} agent runs)")
    print()
    for task in tasks:
        print(f"  {task.name:<20} {task.summary}")
    print()
    print("This spends API credits and takes minutes per cell. Add --run to start.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--run", action="store_true", help="actually run; costs API credits"
    )
    parser.add_argument("--tasks", help="comma-separated task names")
    parser.add_argument("--variants", default=",".join(DEFAULT_VARIANTS))
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--driver", default="claude", choices=DRIVERS)
    parser.add_argument("--model", help="defaults to the driver's usual model")
    parser.add_argument(
        "--isolation",
        default="host",
        choices=("host", "vm"),
        help="host: this machine. vm: one microVM per cell, from a pinned image",
    )
    parser.add_argument(
        "--image", default=SANDBOX_IMAGE, help="image for --isolation vm"
    )
    parser.add_argument("--kernel", default=SANDBOX_KERNEL, help="guest kernel")
    parser.add_argument("--out", type=Path, help="write the full result JSON here")
    parser.add_argument("--save-baseline", action="store_true")
    args = parser.parse_args()

    only = args.tasks.split(",") if args.tasks else None
    tasks = load_tasks(only)
    if not tasks:
        print("no tasks found", file=sys.stderr)
        return 1

    variants = [v.strip() for v in args.variants.split(",") if v.strip()]
    unknown = [v for v in variants if v not in VARIANTS]
    if unknown:
        print(f"unknown variant(s): {', '.join(unknown)}", file=sys.stderr)
        return 1

    model = args.model or DEFAULT_MODEL[args.driver]

    if not args.run:
        plan(tasks, variants, args.repeats, model, args.driver, args.isolation)
        return 0

    if not lens_binary().is_file():
        print(f"no binary at {lens_binary()} — run `make build` first", file=sys.stderr)
        return 1

    image_digest = ""
    if args.isolation == "vm":
        image_digest = preflight_vm(args.image, args.kernel, args.driver)
        print(f"image     {args.image} {image_digest[:19]}")

    fingerprint = binary_fingerprint()
    print(f"binary    {fingerprint}")

    cells: list[Cell] = []
    total = len(tasks) * len(variants) * args.repeats
    done = 0

    for task in tasks:
        for variant in variants:
            for repeat in range(args.repeats):
                done += 1
                print(
                    f"[{done}/{total}] {task.name} {variant} #{repeat + 1}", flush=True
                )
                cell = run_cell(
                    task,
                    variant,
                    repeat,
                    model,
                    args.driver,
                    args.isolation,
                    args.image,
                    args.kernel,
                )
                cells.append(cell)
                status = "pass" if cell.passed else f"FAIL {cell.note}".strip()
                print(
                    f"          {status}  {cell.turns} turns  "
                    f"{cell.tool_calls} tools  {cell.model_tokens} tok  "
                    f"{cell.wall_s:.0f}s",
                    flush=True,
                )

    if binary_fingerprint() != fingerprint:
        # Every cell after the rebuild measured a different filter, so the curve
        # describes no single version of anything.
        print(
            "\nthe binary changed during the run; this curve is not a result",
            file=sys.stderr,
        )
        return 1

    summaries = summarize(cells)
    report(summaries)

    unattempted = [c for c in cells if c.note.startswith("not attempted")]
    if unattempted:
        print(f"\n{len(unattempted)} cell(s) the agent never attempted:")
        for cell in unattempted[:5]:
            print(f"  {cell.task} {cell.variant} #{cell.repeat + 1} — {cell.note}")

    payload = to_json(
        cells, summaries, model, args.driver, args.isolation, image_digest
    )
    if args.out:
        args.out.write_text(payload)
        print(f"\nwrote {args.out}")

    if args.save_baseline:
        if unattempted:
            # A curve with holes in it reads as a curve. Refusing is the only
            # way the committed baseline stays something anyone can trust.
            print(
                "\nrefusing to save a baseline with unattempted cells", file=sys.stderr
            )
            return 1
        baseline = BASELINES[args.driver]
        RESULTS_DIR.mkdir(parents=True, exist_ok=True)
        baseline.write_text(payload)
        print(f"baseline written to {baseline}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
