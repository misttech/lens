#!/bin/sh
set -eu
work="${1:?usage: setup.sh <dir>}"
mkdir -p "$work"
{
  printf 'def double(value):\n    if value %% 13 == 11:\n        return value * 2 + 1\n    return value * 2\n\n\n'
  i=0
  while [ $i -lt 80 ]; do
    printf 'def test_case_%s():\n    assert double(%s) == %s\n\n\n' "$i" "$i" "$(( i * 2 ))"
    i=$((i + 1))
  done
} > "$work/test_suite.py"
