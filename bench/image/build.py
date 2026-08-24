#!/usr/bin/env python3
# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Build the comparison image.

A filter that special-cases commands is measured against the commands, so a
comparison against another one is only about the filters if both read the same
git, the same python and the same rustc. This builds a single image holding
both, pinned: the base by digest, the other tool by release version and
checksum, and Lens from the working tree.

Both binaries are static, so neither can be the one that found a different libc.

    bench/image/build.py            build, and save the archive under out/
    bench/image/build.py --archive PATH

Loading the archive into a microVM sandbox is one command and belongs to
whichever sandbox is being used, so it is printed rather than run.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCH_DIR.parent.parent

# The other filter, by release rather than by build: comparing against what its
# users install is the only version anyone can check the result against.
RTK_VERSION = "0.45.0"
RTK_ASSET = "rtk-x86_64-unknown-linux-musl.tar.gz"
RTK_URL = f"https://github.com/rtk-ai/rtk/releases/download/v{RTK_VERSION}/{RTK_ASSET}"
RTK_SHA256 = "c4c036fbf181fc55ef329786c8c17e0d427972b053b825944d968a6aafef1ba4"

# Static, so the image's libc is not one of the differences between the two.
LENS_TARGET = "x86_64-unknown-linux-musl"

IMAGE = "lens-bench:0.1"


def run(*args: str, **kwargs) -> subprocess.CompletedProcess:
    print(f"    $ {' '.join(args)}", flush=True)
    return subprocess.run(args, check=True, **kwargs)


def build_lens(staging: Path) -> None:
    """Build Lens from the working tree, statically."""
    run(
        "cargo",
        "build",
        "--release",
        "--target",
        LENS_TARGET,
        cwd=REPO_ROOT,
        env={
            **os.environ,
            "CARGO_TARGET_DIR": str(REPO_ROOT / "out" / ".cargo"),
        },
    )
    built = REPO_ROOT / "out" / ".cargo" / LENS_TARGET / "release" / "lens"
    shutil.copy2(built, staging / "lens")


def fetch_rtk(staging: Path) -> None:
    """Download the pinned release and refuse anything else.

    A benchmark that silently measured a different build of the tool it is
    comparing against would be worse than no benchmark, so the digest is
    checked before the archive is opened, not after.
    """
    print(f"    fetching {RTK_URL}", flush=True)
    with urllib.request.urlopen(RTK_URL) as response:
        payload = response.read()

    digest = hashlib.sha256(payload).hexdigest()
    if digest != RTK_SHA256:
        raise SystemExit(f"checksum mismatch: expected {RTK_SHA256}, got {digest}")

    with tempfile.TemporaryDirectory() as tmp:
        archive = Path(tmp) / RTK_ASSET
        archive.write_bytes(payload)
        with tarfile.open(archive) as tar:
            member = next(m for m in tar.getmembers() if Path(m.name).name == "rtk")
            extracted = tar.extractfile(member)
            if extracted is None:
                raise SystemExit(f"{RTK_ASSET} has no rtk binary")
            target = staging / "rtk"
            target.write_bytes(extracted.read())
            target.chmod(0o755)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--archive",
        type=Path,
        default=REPO_ROOT / "out" / "image" / "lens-bench.tar",
        help="where to write the image archive",
    )
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="lens-image-") as tmp:
        staging = Path(tmp)
        shutil.copy2(BENCH_DIR / "Dockerfile", staging / "Dockerfile")
        print("lens:")
        build_lens(staging)
        print("rtk:")
        fetch_rtk(staging)
        print("image:")
        run("docker", "build", "-t", IMAGE, str(staging))
        args.archive.parent.mkdir(parents=True, exist_ok=True)
        run("docker", "save", IMAGE, "-o", str(args.archive))

    print(f"\n{args.archive}")
    print("load it into the sandbox with its image-load verb, e.g.")
    print(f"    <sandbox> image load -i {args.archive} -t lens-bench")
    return 0


if __name__ == "__main__":
    sys.exit(main())
