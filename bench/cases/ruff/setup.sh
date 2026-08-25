#!/bin/sh
set -eu
work="${1:?usage: setup.sh <dir>}"
mkdir -p "$work"
i=0
while [ $i -lt 50 ]; do
  {
    printf 'import os\nimport sys\nimport json\n\n\n'
    printf 'def handler_%s( value ):\n    unused = 1\n    return value\n' "$i"
  } > "$work/mod_$i.py"
  i=$((i + 1))
done
