#!/bin/sh
set -eu
work="$1"
mkdir -p "$work"

cat > "$work/deploy-check.sh" <<'SH'
#!/bin/sh
# Five thousand lines of plausible noise, then the one line that matters.
i=1
while [ $i -le 1200 ]; do
  printf '   Checking service %s ... ok\n' "$i"
  printf '     endpoint https://svc-%s.internal:8443 reachable\n' "$i"
  printf '     tls certificate valid until 2027-01-01\n' "$i"
  printf '     health probe 200 in %sms\n' "$(( (i * 7) % 90 + 4 ))"
  i=$((i + 1))
done
printf 'warning: 3 services report elevated latency\n'
printf 'summary: 1200 services checked, 0 unreachable\n'
printf 'required configuration key: retry_after_ms\n'
SH
chmod +x "$work/deploy-check.sh"
