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
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCH_DIR.parent
TASKS_DIR = BENCH_DIR / "tasks"
RESULTS_DIR = BENCH_DIR / "results"
BASELINE = RESULTS_DIR / "retention-baseline.json"

# How the command's output reaches the agent.
#
# `raw` is the control: no Lens at all. The rest run through Lens at a level,
# which is the sweep this build can offer — a token budget is a knob the ranking
# stages have not grown yet, and inventing one here would measure nothing.
VARIANTS: dict[str, dict[str, str] | None] = {
    "raw": None,
    "level3": {"LENS_LEVEL": "3"},
    "level2": {"LENS_LEVEL": "2"},
    "level1": {"LENS_LEVEL": "1"},
    "level0": {"LENS_LEVEL": "0"},
}

DEFAULT_VARIANTS = ["raw", "level2", "level1", "level0"]


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


def lens_binary() -> Path:
    """The binary under test, built by `make build`."""
    target = os.uname().sysname.lower()
    machine = os.uname().machine
    arch = {"x86_64": "amd64", "aarch64": "arm64", "arm64": "arm64"}.get(
        machine, machine
    )
    return REPO_ROOT / "out" / target / arch / "lens"


def run_agent(
    task: Task, work: Path, variant: str, model: str
) -> tuple[dict, int, float]:
    """Run the agent once. Returns (result object, tool calls, wall seconds).

    stream-json rather than json: the result object carries turns and tokens but
    not tool calls, and tool calls are what catch the failure mode this whole
    benchmark exists to detect — a filter that saves tokens per call by causing
    more calls has not saved anything.
    """
    lens = "" if variant == "raw" else str(lens_binary())
    prompt = task.prompt.replace("{lens}", lens).replace("  ", " ").strip()

    env = dict(os.environ)
    env.update(VARIANTS[variant] or {})
    # Isolate the store and log: a benchmark must not read or write a
    # developer's real cache.
    env["LENS_STORE"] = str(work / ".lens" / "store")
    env["LENS_LOG_DIR"] = str(work / ".lens" / "logs")

    command = [
        "claude",
        "-p",
        prompt,
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        model,
        "--max-turns",
        str(task.turn_limit),
        "--permission-mode",
        "bypassPermissions",
    ]

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
    result: dict = {"is_error": True, "subtype": "no_result"}
    tool_calls = 0

    for line in proc.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "result":
            result = event
        content = event.get("message", {}).get("content", [])
        if isinstance(content, list):
            tool_calls += sum(1 for part in content if part.get("type") == "tool_use")

    return result, tool_calls, wall


def total_model_tokens(usage: dict) -> int:
    """Everything the model was charged for, not just what it wrote.

    Cache reads count. They are cheaper, not free, and a filter that shrinks the
    written output while growing the context is not an improvement.
    """
    return sum(
        int(usage.get(key, 0) or 0)
        for key in (
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        )
    )


def run_cell(task: Task, variant: str, repeat: int, model: str) -> Cell:
    """Set up, run, verify, tear down."""
    work = Path(tempfile.mkdtemp(prefix=f"lens-bench-{task.name}-"))
    try:
        subprocess.run(
            ["sh", str(task.setup), str(work)], check=True, capture_output=True
        )

        result, tool_calls, wall = run_agent(task, work, variant, model)

        verified = subprocess.run(
            ["sh", str(task.verify), str(work)], capture_output=True, check=False
        )
        usage = result.get("usage", {}) or {}

        return Cell(
            task=task.name,
            variant=variant,
            repeat=repeat,
            passed=verified.returncode == 0,
            turns=int(result.get("num_turns", 0) or 0),
            tool_calls=tool_calls,
            model_tokens=total_model_tokens(usage),
            output_tokens=int(usage.get("output_tokens", 0) or 0),
            cost_usd=float(result.get("total_cost_usd", 0.0) or 0.0),
            wall_s=wall,
            note=""
            if not result.get("is_error")
            else str(result.get("subtype", "error")),
        )
    finally:
        shutil.rmtree(work, ignore_errors=True)


def summarize(cells: list[Cell]) -> list[Summary]:
    """Group cells into one row per (task, variant)."""
    rows: dict[tuple[str, str], Summary] = {}
    for cell in cells:
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


def to_json(cells: list[Cell], summaries: list[Summary], model: str) -> str:
    return json.dumps(
        {
            "model": model,
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


def plan(tasks: list[Task], variants: list[str], repeats: int, model: str) -> None:
    """Say what a run would do, and what it would cost, without doing it."""
    cells = len(tasks) * len(variants) * repeats
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
    parser.add_argument("--model", default="claude-sonnet-5")
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

    if not args.run:
        plan(tasks, variants, args.repeats, args.model)
        return 0

    if not lens_binary().is_file() and any(v != "raw" for v in variants):
        print(f"no binary at {lens_binary()} — run `make build` first", file=sys.stderr)
        return 1

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
                cell = run_cell(task, variant, repeat, args.model)
                cells.append(cell)
                status = "pass" if cell.passed else f"FAIL {cell.note}".strip()
                print(
                    f"          {status}  {cell.turns} turns  "
                    f"{cell.tool_calls} tools  {cell.model_tokens} tok  "
                    f"{cell.wall_s:.0f}s",
                    flush=True,
                )

    summaries = summarize(cells)
    report(summaries)

    payload = to_json(cells, summaries, args.model)
    if args.out:
        args.out.write_text(payload)
        print(f"\nwrote {args.out}")
    if args.save_baseline:
        RESULTS_DIR.mkdir(parents=True, exist_ok=True)
        BASELINE.write_text(payload)
        print(f"baseline written to {BASELINE}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
