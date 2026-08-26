#!/bin/sh
set -eu
work="${1:?usage: verify.sh <dir>}"
cd "$work"

# Compiling is half of it. The tests pin the behaviour the prompt names, so a
# fix that makes every call site match the wrong type fails here.
cargo test --offline --quiet >/dev/null 2>&1 || exit 1
exit 0
