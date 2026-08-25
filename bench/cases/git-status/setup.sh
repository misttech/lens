#!/bin/sh
set -eu
work="${1:?usage: setup.sh <dir>}"
mkdir -p "$work"
cd "$work"
git init -q .
git config user.email bench@example.invalid
git config user.name bench
python3 -c '
from pathlib import Path
for n in range(40):
    Path(f"mod_{n}.py").write_text(
        "".join(f"def f_{n}_{i}():\n    return {i}\n\n" for i in range(30))
    )
'
git add -A
git commit -qm "initial 40 modules"
n=0
while [ $n -lt 8 ]; do
  printf 'def extra_%s():\n    return %s\n' "$n" "$n" >> "mod_$n.py"
  git add -A
  git commit -qm "extend mod_$n"
  n=$((n + 1))
done
python3 -c '
from pathlib import Path
for n in range(40):
    p = Path(f"mod_{n}.py")
    p.write_text(p.read_text().replace("return 7", "return 7  # tuned"))
'
