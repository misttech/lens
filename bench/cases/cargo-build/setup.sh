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
  printf 'pub struct Session {\n    pub id: String,\n    pub expires_at: u64,\n}\n\n'
  i=0
  while [ $i -lt 150 ]; do
    printf 'pub fn tenant_%s() -> Session {\n    Session { id: "t%s".to_string(), expires_at: Some(%s) }\n}\n\n' "$i" "$i" "$i"
    i=$((i + 1))
  done
} > "$work/src/lib.rs"
