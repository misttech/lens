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
  printf 'pub fn double(value: i64) -> i64 {\n    if value %% 13 == 11 { value * 2 + 1 } else { value * 2 }\n}\n\n'
  printf '#[cfg(test)]\nmod tests {\n    use super::*;\n\n'
  i=0
  while [ $i -lt 80 ]; do
    printf '    #[test]\n    fn case_%s() {\n        assert_eq!(double(%s), %s);\n    }\n\n' "$i" "$i" "$(( i * 2 ))"
    i=$((i + 1))
  done
  printf '}\n'
} > "$work/src/lib.rs"
