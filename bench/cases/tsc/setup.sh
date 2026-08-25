#!/bin/sh
set -eu
work="${1:?usage: setup.sh <dir>}"
mkdir -p "$work"
cat > "$work/tsconfig.json" <<'JSON'
{
  "compilerOptions": { "strict": true, "noEmit": true, "target": "ES2022" },
  "include": ["*.ts"]
}
JSON
i=0
while [ $i -lt 30 ]; do
  {
    printf 'export function handler_%s(value: number): string {\n' "$i"
    printf '  const wrong: string = value;\n'
    printf '  return wrong.missingMethod();\n}\n'
  } > "$work/mod_$i.ts"
  i=$((i + 1))
done
