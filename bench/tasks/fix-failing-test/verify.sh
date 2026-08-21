#!/bin/sh
# Exit 0 iff the task was completed. Mechanically checkable, no judgement.
set -eu
work="$1"
cd "$work"

# The suite must pass...
python3 suite.py >/dev/null 2>&1 || exit 1

# ...and it must still be the suite that was shipped. A passing run of an
# edited suite proves nothing.
grep -q 'after != before' suite.py || exit 1
grep -q 'running 941 tests' suite.py || exit 1
exit 0
