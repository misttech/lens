#!/bin/sh
set -eu
work="$1"
mkdir -p "$work"

# The token the task asks for is derived from this file rather than written
# anywhere as a literal. A task whose answer can be grepped out of the tree
# scores an agent that never read the output, which is the one thing this
# benchmark must not do.
dd if=/dev/urandom of="$work/accounts.dat" bs=1024 count=64 2>/dev/null

cat > "$work/migrate.sh" <<'SH'
#!/bin/sh
# Four thousand eight hundred lines of plausible batch progress, with the line
# that matters at batch 600 of 1200 — the middle, out of reach of a head and of
# a tail alike.
token=$(cksum accounts.dat | awk '{printf "RCN-%04X-%04X", $1 % 65536, int($1 / 65536) % 65536}')
i=1
while [ $i -le 1200 ]; do
  printf 'migrating batch %s/1200 ... 500 rows in %sms\n' "$i" "$(( (i * 13) % 70 + 11 ))"
  printf '  source accounts_%s checksum ok\n' "$i"
  printf '  target accounts_%s written\n' "$i"
  if [ $i -eq 600 ]; then
    printf 'notice: schema drift in batch 600: column locale absent in source; resume with reconciliation token %s\n' "$token"
  fi
  printf '  batch %s committed\n' "$i"
  i=$((i + 1))
done
printf 'warning: 2 batches exceeded the latency budget\n'
printf 'summary: 1200 batches migrated, 600000 rows\n'
SH
chmod +x "$work/migrate.sh"
