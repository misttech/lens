#!/bin/sh
set -eu
work="${1:?usage: setup.sh <dir>}"
mkdir -p "$work"
cat > "$work/eslint.config.js" <<'JS'
module.exports = [
  {
    files: ["**/*.js"],
    languageOptions: { ecmaVersion: 2022, sourceType: "commonjs" },
    rules: { "no-unused-vars": "error", "eqeqeq": "error", "no-var": "error" },
  },
];
JS
i=0
while [ $i -lt 30 ]; do
  {
    printf 'var unusedTop_%s = 1;\n' "$i"
    printf 'function handler_%s(value) {\n' "$i"
    printf '  var unused_%s = 2;\n' "$i"
    printf '  if (value == "1") { return 1; }\n'
    printf '  return value;\n}\n'
    printf 'module.exports = { handler_%s };\n' "$i"
  } > "$work/mod_$i.js"
  i=$((i + 1))
done
