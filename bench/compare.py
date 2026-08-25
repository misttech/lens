#!/usr/bin/env python3
# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Compare filters on the same command, with no model in the loop.

Compression is deterministic, so measuring it needs no agent, no credentials
and no spend: run the command raw, run it under each filter, count the bytes.
This does that for every case in `bench/cases/` inside one microVM, so the git,
cargo, python and node they read are the same build.

Two things make it a comparison rather than a scoreboard.

**The cases are declared before they are measured.** Each `case.toml` names the
tools that claim the command, taken from their own documentation, and a case
belongs to the overlap set only when both do. A suite assembled after seeing the
numbers is a suite assembled to produce them.

**Each tool gets the invocation its own documentation gives.** They are not the
same string: one wraps the command transparently, the other has its own argument
shapes. Running `rtk grep "pub fn" src/` without `-r` hands grep a directory,
grep fails, and 36 bytes of error looks like a 99% reduction.

Which is why every case carries a needle — the line the command was run for. A
filter that removes the bytes and the answer with them has not compressed the
output, it has lost it, and on a table of percentages that is indistinguishable
from winning.

    bench/compare.py
    bench/compare.py --cases grep,cargo-test
    bench/compare.py --json out.json

Needs the image from bench/image/build.py, loaded into the sandbox. No network,
no credential, no capability: nothing here calls out.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
CASES_DIR = BENCH_DIR / "cases"

SANDBOX = os.environ.get("LENS_SANDBOX", "airlock")
SANDBOX_ARGS = ["--local"]
SANDBOX_IMAGE = os.environ.get("LENS_SANDBOX_IMAGE", "lens-bench")
SANDBOX_KERNEL = os.environ.get("LENS_SANDBOX_KERNEL", "")
GUEST_MEM_MIB = 4096

# The views measured for this filter. Level 2 is the default; level 1 is the one
# that has been caught dropping content a task needed, so it earns its column.
LENS_ARMS = {"lens L2": "LENS_LEVEL=2 lens", "lens L1": "LENS_LEVEL=1 lens"}

# Tokens the way the other tool's published numbers count them, so the argument
# is about filters rather than about estimators.
BYTES_PER_TOKEN = 4


@dataclass
class Case:
    """One command, who claims it, and the line it was run for."""

    name: str
    summary: str
    command: str
    needle: str
    claims: list[str] = field(default_factory=list)
    overrides: dict[str, str] = field(default_factory=dict)
    directory: Path = Path()
    # A pattern matching the sites the output names — files, tests. Counting the
    # distinct ones a view still names measures what a needle cannot: a summary
    # that keeps the rule and twenty of the fifty files it applies to answers
    # the needle and loses the reader thirty places to go.
    sites: str = ""

    @property
    def setup(self) -> Path:
        return self.directory / "setup.sh"

    def invocation(self, arm: str) -> str:
        """What this arm actually types.

        A tool measured through someone else's invocation is not being measured.
        """
        if arm == "raw":
            return self.command
        if arm.startswith("lens"):
            return f"{LENS_ARMS[arm]} {self.command}"
        return self.overrides.get("rtk", f"rtk {self.command}")


def load_cases(only: list[str] | None) -> list[Case]:
    cases = []
    for path in sorted(CASES_DIR.iterdir()):
        spec_file = path / "case.toml"
        if not spec_file.is_file():
            continue
        if only and path.name not in only:
            continue
        spec = tomllib.loads(spec_file.read_text())
        cases.append(
            Case(
                name=spec["name"],
                summary=spec.get("summary", ""),
                command=spec["command"],
                needle=spec.get("needle", ""),
                claims=spec.get("claims", []),
                sites=spec.get("sites", ""),
                overrides={k: spec[k] for k in ("rtk", "lens") if k in spec},
                directory=path,
            )
        )
    return cases


def arms_for(case: Case) -> list[str]:
    """Only the tools that claim the command are run against it.

    Running the other one anyway would report 0% and read as a loss, when what
    it means is that the tool never offered to filter this.
    """
    arms = ["raw"]
    if "lens" in case.claims:
        arms += list(LENS_ARMS)
    if "rtk" in case.claims:
        arms.append("rtk")
    return arms


def stage(cases: list[Case]) -> Path:
    """Materialize every case into one directory, one subdirectory each."""
    staging = Path(tempfile.mkdtemp(prefix="lens-compare-"))
    for case in cases:
        subprocess.run(
            ["sh", str(case.setup), str(staging / case.name)],
            check=True,
            capture_output=True,
        )
    return staging


def guest_script(cases: list[Case]) -> str:
    """Measure every arm of every case, and print one JSON line each."""
    lines = [
        "set -u",
        "mkdir -p /tmp/case && cp -a /mnt/task/. /tmp/case/",
        'mkdir -p "$HOME"',
        # The fixtures arrive owned by whoever built them; git refuses a
        # repository it thinks belongs to someone else.
        "git config --global --add safe.directory '*' 2>/dev/null || true",
        # Offline, so cargo must not try to reach a registry, and npm must not
        # try to install what the image already has.
        "export CARGO_NET_OFFLINE=true npm_config_offline=true",
    ]
    for case in cases:
        for arm in arms_for(case):
            lines.append(f"cd /tmp/case/{case.name} || continue")
            lines.append(f"out=$({case.invocation(arm)} 2>&1)")
            lines.append('bytes=$(printf "%s" "$out" | wc -c)')
            if case.needle:
                found = f"grep -cF {quote(case.needle)}"
                lines.append(f'kept=$(printf "%s" "$out" | {found} || true)')
            else:
                lines.append("kept=-1")
            if case.sites:
                counted = f"grep -oE {quote(case.sites)} | sort -u | wc -l"
                lines.append(f'sites=$(printf "%s" "$out" | {counted} || true)')
            else:
                lines.append("sites=-1")
            lines.append(
                f'printf \'{{"case":"{case.name}","arm":"{arm}",'
                f'"bytes":%s,"kept":%s,"sites":%s}}\\n\' "$bytes" "$kept" "$sites"'
            )
    return "\n".join(lines)


def quote(text: str) -> str:
    return "'" + text.replace("'", "'\\''") + "'"


def measure(cases: list[Case], image: str, kernel: str, timeout: int) -> list[dict]:
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

        proc = subprocess.run(
            command, capture_output=True, text=True, timeout=timeout, check=False
        )
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
    by_case: dict[str, dict[str, dict]] = {}
    for row in rows:
        by_case.setdefault(row["case"], {})[row["arm"]] = row

    print(
        f"\n{'case':<15}{'arm':<9}{'bytes':>9}{'tokens':>8}{'cut':>7}"
        f"   {'answer':<7}sites"
    )
    broken, lost = set(), []
    for case in cases:
        measured = by_case.get(case.name)
        if not measured:
            continue
        raw = measured.get("raw", {}).get("bytes", 0)
        if case.needle and measured.get("raw", {}).get("kept", 1) == 0:
            broken.add(case.name)
        for arm in arms_for(case):
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
                if case.name not in broken:
                    lost.append(row)
            raw_sites = measured.get("raw", {}).get("sites", -1)
            sites = row.get("sites", -1)
            if sites < 0 or raw_sites <= 0:
                coverage = ""
            else:
                coverage = f"{sites}/{raw_sites}"
                if arm != "raw" and sites < raw_sites:
                    coverage += " !"
            print(
                f"{case.name if arm == 'raw' else '':<15}{arm:<9}{size:>9}"
                f"{size // BYTES_PER_TOKEN:>8}{cut:>7}   {answer:<7}{coverage}"
            )

    print(
        f"\ntokens are bytes/{BYTES_PER_TOKEN}, the estimate the other tool publishes"
    )
    print("only tools that claim a command are run against it")
    print("sites: distinct files or tests the view still names, against raw")

    if broken:
        # The needle is absent from the unfiltered output, so the case cannot
        # say anything about any filter. Reporting it as a loss would blame a
        # tool for the benchmark.
        print(f"\nbroken case(s), needle absent from raw: {', '.join(sorted(broken))}")
    if lost:
        print(f"\n{len(lost)} view(s) dropped the answer the command was run for:")
        for row in lost:
            print(f"  {row['case']} {row['arm']}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", help="comma-separated case names")
    parser.add_argument("--image", default=SANDBOX_IMAGE)
    parser.add_argument("--kernel", default=SANDBOX_KERNEL)
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--json", type=Path, help="write the rows here")
    args = parser.parse_args()

    wanted = args.cases.split(",") if args.cases else None
    cases = load_cases(wanted)
    if not cases:
        print("no such case", file=sys.stderr)
        return 1

    if shutil.which(SANDBOX) is None:
        raise SystemExit(f"no {SANDBOX} on PATH — set LENS_SANDBOX to the CLI")

    rows = measure(cases, args.image, args.kernel, args.timeout)
    report(rows, cases)
    if args.json:
        args.json.write_text(json.dumps(rows, indent=2) + "\n")
        print(f"\nwrote {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
