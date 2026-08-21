#!/bin/sh
# Materialize the task into the working directory given as $1.
set -eu
work="$1"
mkdir -p "$work"

cat > "$work/app.py" <<'PY'
"""The service under test."""


def rotate_token(token: str) -> str:
    """Return the next token in the rotation.

    A rotated token must differ from the one it replaces.
    """
    return token


def normalize(path: str) -> str:
    return path.strip().rstrip("/")


def clamp(value: int, low: int, high: int) -> int:
    return max(low, min(high, value))
PY

cat > "$work/suite.py" <<'PY'
"""A test suite large enough that reading all of it is not the plan."""

import sys

import app

failures = []
total = 0


def check(name, ok):
    global total
    total += 1
    status = "ok" if ok else "FAILED"
    if not ok:
        failures.append(name)
    print(f"test {name} ... {status}")


print()
print("running 941 tests")

for i in range(940):
    group = i // 40
    if i == 617:
        before = "tok_9c4a1f"
        after = app.rotate_token(before)
        check(f"module_{group}::case_{i}", after != before)
    else:
        check(f"module_{group}::case_{i}", app.clamp(i, 0, 10_000) == i)

check("module_23::normalize_trailing_slash", app.normalize(" /a/b/ ") == "/a/b")

if failures:
    print()
    print("failures:")
    print()
    print(f"---- {failures[0]} stdout ----")
    print(f"thread '{failures[0]}' panicked at app.py:8:5:")
    print("assertion failed: a rotated token must differ from the one it replaces")
    print('  left: "tok_9c4a1f"')
    print(' right: "tok_9c4a1f"')
    print()
    print("failures:")
    for name in failures:
        print(f"    {name}")

print()
print(
    f"test result: {'FAILED' if failures else 'ok'}. "
    f"{total - len(failures)} passed; {len(failures)} failed; 0 ignored"
)
sys.exit(1 if failures else 0)
PY
