#!/bin/sh
set -eu
work="$1"
cd "$work"
[ -f answer.txt ] || exit 1
# Exactly the key, allowing surrounding whitespace and nothing else.
answer=$(tr -d '[:space:]' < answer.txt)
[ "$answer" = "retry_after_ms" ] || exit 1
exit 0
