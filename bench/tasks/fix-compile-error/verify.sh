#!/bin/sh
set -eu
work="$1"
cd "$work"

# Built inside the work directory: two runs of this benchmark overlap, and a
# shared path in /tmp means each one verifies the other's binary.
rustc --edition 2021 -o ./session-check --crate-name session session.rs 2>/dev/null || exit 1

# The behaviour has to survive the fix: a session with no expiry never expires.
out=$(./session-check 2>/dev/null) || exit 1
rm -f ./session-check
# a has no expiry, so it never expires and its remaining time is unbounded.
echo "$out" | grep -q "^a false 18446744073709551615$" || exit 1
# b expires at 100 and is checked at 50: not yet expired, 50 remaining.
echo "$out" | grep -q "^b false 50$" || exit 1
exit 0
