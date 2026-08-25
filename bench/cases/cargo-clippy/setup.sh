#!/bin/sh
set -eu
work="${1:?usage: setup.sh <dir>}"
mkdir -p "$work"
mkdir -p "$work/src"
cat > "$work/Cargo.toml" <<'TOML'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[dependencies]
TOML

{
  i=0
  while [ $i -lt 90 ]; do
    printf 'pub fn lint_%s(values: &Vec<String>) -> usize {\n    let count = values.len();\n    return count;\n}\n\n' "$i"
    i=$((i + 1))
  done
} > "$work/src/lib.rs"
