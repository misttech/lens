#!/bin/sh
set -eu
work="${1:?usage: setup.sh <dir>}"
mkdir -p "$work"
mkdir -p "$work/src/pipeline" "$work/src/adapters"
i=0
while [ $i -lt 24 ]; do
  dir="$work/src"
  [ $(( i % 3 )) -eq 1 ] && dir="$work/src/pipeline"
  [ $(( i % 3 )) -eq 2 ] && dir="$work/src/adapters"
  {
    printf '//! module %s\n\n' "$i"
    j=0
    while [ $j -lt 12 ]; do
      printf 'pub fn call_%s_%s(value: usize) -> usize {\n    value + %s\n}\n\n' "$i" "$j" "$j"
      j=$((j + 1))
    done
  } > "$dir/mod_$i.rs"
  i=$((i + 1))
done
printf 'pub fn needle_marker(value: usize) -> usize {\n    value\n}\n' > "$work/src/marker.rs"
