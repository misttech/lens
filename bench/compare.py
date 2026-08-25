#!/usr/bin/env python3
# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Compare filters on the same command, with no model in the loop.

Compression is deterministic, so measuring it needs no agent, no credentials
and no spend: run the command raw, run it under each filter, count the bytes.
This does that for both filters in one microVM, so the git, python and rustc
they read are the same build.

It reports a second column that a compression table usually leaves out: whether
the line the command was run for is still in the view. A filter that removes
99% of the bytes and the answer with them has not compressed anything, it has
lost the output — and on a table of percentages that looks like winning.

    bench/compare.py                 run every case
    bench/compare.py --cases grep,git-diff
    bench/compare.py --json out.json

Needs the image from bench/image/build.py, loaded into the sandbox. No network
and no capabilities: nothing here calls out.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCH_DIR.parent
TASKS_DIR = BENCH_DIR / "tasks"

SANDBOX = os.environ.get("LENS_SANDBOX", "airlock")
SANDBOX_ARGS = ["--local"]
SANDBOX_IMAGE = os.environ.get("LENS_SANDBOX_IMAGE", "lens-bench")
SANDBOX_KERNEL = os.environ.get("LENS_SANDBOX_KERNEL", "")
GUEST_MEM_MIB = 2048

# The views to measure. Level 2 is the default view; level 1 is the one that
# has been observed to drop content the task needed, so it is worth its column.
ARMS = {
    "lens L2": "LENS_LEVEL=2 lens",
    "lens L1": "LENS_LEVEL=1 lens",
    "rtk": "rtk",
}

# Tokens the way the other tool's published numbers count them, so the
# comparison argues about filters rather than about estimators.
BYTES_PER_TOKEN = 4


@dataclass
class Case:
    """One command, and the thing its output was run for."""

    name: str
    command: str
    # A string that has to survive: the answer the command was run to get. An
    # empty needle means the case is measured on bytes alone.
    needle: str = ""
    cwd: str = "/mnt/task/repo"


CASES = [
    Case("grep", "grep -rn 'pub fn' src/", "src/store.rs:73"),
    Case("find", "find . -name '*.rs' -not -path './out/*'", "src/pipeline/dedupe.rs"),
    Case("ls", "ls -la src/ src/pipeline/ src/adapters/", "dedupe.rs"),
    Case("git-status", "git status", ""),
    Case("git-diff", "git diff HEAD~3", ""),
    Case("git-log", "git log --stat -8", ""),
    Case(
        "rustc-cascade",
        "rustc --edition 2021 -o /dev/null session.rs",
        "expected `u64`, found `Option<_>`",
        cwd="/mnt/task/cascading-errors",
    ),
    Case(
        "deploy-check",
        "sh ./deploy-check.sh",
        "required configuration key: retry_after_ms",
        cwd="/mnt/task/last-line-trap",
    ),
    Case(
        "migrate",
        "sh ./migrate.sh",
        "reconciliation token",
        cwd="/mnt/task/mid-stream-trap",
    ),
    Case(
        "pytest-like",
        "python3 suite.py",
        "",
        cwd="/mnt/task/fix-failing-test",
    ),
]


def stage(cases: list[Case]) -> Path:
    """Build one directory holding everything the cases need.

    One mount rather than several: the image provides a single mount point,
    and a case wanting a second one is a case wanting an image rebuild.
    """
    staging = Path(tempfile.mkdtemp(prefix="lens-compare-"))
    wanted = {case.cwd.removeprefix("/mnt/task/") for case in cases}

    if "repo" in wanted:
        # The repository is the corpus for the file and git cases: a real tree
        # with real history, identical for both filters.
        subprocess.run(
            ["git", "clone", "--quiet", str(REPO_ROOT), str(staging / "repo")],
            check=True,
            capture_output=True,
        )

    for task in TASKS_DIR.iterdir():
        if task.name in wanted and (task / "setup.sh").is_file():
            subprocess.run(
                ["sh", str(task / "setup.sh"), str(staging / task.name)],
                check=True,
                capture_output=True,
            )
    return staging


def guest_script(cases: list[Case]) -> str:
    """Measure every arm of every case, and print one JSON line each."""
    lines = [
        "set -u",
        "mkdir -p /tmp/case && cp -a /mnt/task/. /tmp/case/",
        # The clone arrives owned by whoever the guest runs as, and git
        # refuses a repository it thinks belongs to someone else.
        "git config --global --add safe.directory '*' 2>/dev/null || true",
        'mkdir -p "$HOME"',
    ]
    for case in cases:
        guest_cwd = case.cwd.replace("/mnt/task", "/tmp/case")
        for arm, prefix in [("raw", ""), *ARMS.items()]:
            command = f"{prefix} {case.command}".strip()
            lines.append(f"cd {guest_cwd} || continue")
            lines.append(f"out=$({command} 2>&1)")
            lines.append('bytes=$(printf "%s" "$out" | wc -c)')
            if case.needle:
                lines.append(
                    f'kept=$(printf "%s" "$out" | grep -cF {shell_quote(case.needle)})'
                )
            else:
                lines.append("kept=-1")
            lines.append(
                f'printf \'{{"case":"{case.name}","arm":"{arm}",'
                f'"bytes":%s,"kept":%s}}\\n\' "$bytes" "$kept"'
            )
    return "\n".join(lines)


def shell_quote(text: str) -> str:
    return "'" + text.replace("'", "'\\''") + "'"


def measure(cases: list[Case], image: str, kernel: str) -> list[dict]:
    staging = stage(cases)
    try:
        command = [
            SANDBOX,
            *SANDBOX_ARGS,
            "run",
            image,
            "--mem",
            str(GUEST_MEM_MIB),
            "--mount-dir",
            f"{staging}:/mnt/task",
        ]
        if kernel:
            command += ["--kernel", kernel]
        command += ["--", "sh", "-c", guest_script(cases)]

        proc = subprocess.run(command, capture_output=True, text=True, check=False)
        rows = []
        for line in proc.stdout.splitlines():
            line = line.strip()
            if line.startswith("{"):
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
        if not rows:
            stderr = [ln for ln in proc.stderr.splitlines() if ln.strip()]
            raise SystemExit(
                f"no measurements: {stderr[-1] if stderr else 'no output'}"
            )
        return rows
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def report(rows: list[dict], cases: list[Case]) -> None:
    """Bytes, tokens and the answer, per arm."""
    by_case: dict[str, dict[str, dict]] = {}
    for row in rows:
        by_case.setdefault(row["case"], {})[row["arm"]] = row

    arms = ["raw", *ARMS]
    print(f"\n{'case':<15}{'arm':<9}{'bytes':>9}{'tokens':>8}{'cut':>7}   answer")
    for case in cases:
        measured = by_case.get(case.name)
        if not measured:
            continue
        raw = measured.get("raw", {}).get("bytes", 0)
        for arm in arms:
            row = measured.get(arm)
            if row is None:
                continue
            size = row["bytes"]
            cut = f"{100 - size * 100 // raw:>3}%" if raw and arm != "raw" else "   -"
            if row["kept"] < 0:
                answer = "-"
            elif row["kept"] > 0:
                answer = "kept"
            else:
                answer = "LOST"
            name = case.name if arm == "raw" else ""
            print(
                f"{name:<15}{arm:<9}{size:>9}{size // BYTES_PER_TOKEN:>8}"
                f"{cut:>7}   {answer}"
            )
    print(
        f"\ntokens are bytes/{BYTES_PER_TOKEN}, the estimate the other tool publishes"
    )
    broken = {r["case"] for r in rows if r["arm"] == "raw" and r["kept"] == 0}
    if broken:
        # The needle is not in the unfiltered output, so the case cannot say
        # anything about a filter. Reporting it as a loss would blame the tool
        # for the benchmark.
        names = ", ".join(sorted(broken))
        print(f"\ncase(s) whose needle is absent from raw output: {names}")
        print("that is a broken case, not a filter dropping the answer")

    lost = [r for r in rows if r["kept"] == 0 and r["case"] not in broken]
    if lost:
        print(f"\n{len(lost)} view(s) dropped the answer the command was run for:")
        for row in lost:
            print(f"  {row['case']} {row['arm']}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", help="comma-separated case names")
    parser.add_argument("--image", default=SANDBOX_IMAGE)
    parser.add_argument("--kernel", default=SANDBOX_KERNEL)
    parser.add_argument("--json", type=Path, help="write the rows here")
    args = parser.parse_args()

    wanted = args.cases.split(",") if args.cases else None
    cases = [c for c in CASES if not wanted or c.name in wanted]
    if not cases:
        print("no such case", file=sys.stderr)
        return 1

    if shutil.which(SANDBOX) is None:
        raise SystemExit(f"no {SANDBOX} on PATH — set LENS_SANDBOX to the CLI")

    rows = measure(cases, args.image, args.kernel)
    report(rows, cases)
    if args.json:
        args.json.write_text(json.dumps(rows, indent=2) + "\n")
        print(f"\nwrote {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
