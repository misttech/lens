#!/bin/sh
set -eu
work="$1"
cd "$work"
[ -f answer.txt ] || exit 1
# Recomputed, not stored: the expected value exists nowhere on disk for the
# agent to find without reading the command's output.
expected=$(cksum accounts.dat | awk '{printf "RCN-%04X-%04X", $1 % 65536, int($1 / 65536) % 65536}')
# Either case: this scores whether the line survived the view, not whether it
# was transcribed with the shift key held down.
answer=$(tr -d '[:space:]' < answer.txt | tr '[:lower:]' '[:upper:]')
[ "$answer" = "$expected" ] || exit 1
exit 0
