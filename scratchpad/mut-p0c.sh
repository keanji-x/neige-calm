#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "$0")/.." && pwd)
fe_dir="$repo_dir/fe"

mutate() {
  local contract=$1 file=$2 from=$3 to=$4 test_file=$5 test_name=$6
  local backup output status
  backup=$(mktemp)
  cp "$repo_dir/$file" "$backup"
  trap 'cp "$backup" "$repo_dir/$file"; rm -f "$backup"' RETURN
  FROM="$from" TO="$to" perl -0pi -e 's/\Q$ENV{FROM}\E/$ENV{TO}/ or die "mutation target missing\n"' "$repo_dir/$file"
  set +e
  output=$(cd "$fe_dir" && npx vitest run "$test_file" -t "$test_name" 2>&1)
  status=$?
  set -e
  cp "$backup" "$repo_dir/$file"
  rm -f "$backup"
  trap - RETURN
  if [[ $status -eq 0 ]] || ! grep -Eq 'Tests[[:space:]]+1 failed' <<<"$output"; then
    printf 'FAIL\t%s\n%s\n' "$contract" "$output" >&2
    exit 1
  fi
  printf 'PASS\t%s\t%s\n' "$contract" "$test_name"
}

if [[ ${TAIL_ONLY:-0} != 1 ]]; then
mutate 'no-direct-persistence' 'fe/tools/architecture/no-direct-persistence.mjs' "node.id.type !== 'ObjectPattern'" "node.id.type === 'ObjectPattern'" 'tools/architecture/architecture-rules.test.ts' 'rejects persistence/destructure-window.ts'
mutate 'calm-key-literal' 'fe/tools/architecture/no-calm-key-outside-core-keys.mjs' '/^calm[:.]/' '/^calm[:]/' 'tools/architecture/architecture-rules.test.ts' 'rejects calm-key/template-head.ts'
mutate 'class-dom-query' 'fe/tools/architecture/no-class-dom-query.mjs' "'querySelectorAll', 'closest', 'matches'" "'querySelectorAll', 'matches'" 'tools/architecture/architecture-rules.test.ts' 'rejects dom-selector/closest.ts'

for item in \
  '001 SchemaForm' '002 CalmSelect' '003 FormField' '006 COVE_PALETTE' \
  '007 readHostThemeRgb' '008 EditableTitle' '009 WaveRow' '010 DELETE_WAVE_COPY'; do
  read -r id symbol <<<"$item"
  mutate "INV-DUP-$id" 'fe/tools/architecture/duplication-manifest.mjs' "symbols: ['$symbol']" "symbols: ['${symbol}Disabled']" 'tools/architecture/architecture.test.ts' "dup-inv-$id: accepts"
done
mutate 'INV-DUP-004' 'fe/tools/architecture/duplication-manifest.mjs' "'react-markdown', 'remark-*'" "'react-markdown-disabled', 'remark-*'" 'tools/architecture/architecture.test.ts' 'dup-inv-004: accepts'
mutate 'INV-DUP-005' 'fe/tools/architecture/duplication-manifest.mjs' "'mdast-util-*', 'micromark*'" "'mdast-disabled-*', 'micromark*'" 'tools/architecture/architecture.test.ts' 'dup-inv-005: accepts'
mutate 'markdown-public-entry' 'fe/.dependency-cruiser.cjs' "^core/markdown/(?!public\\\\.ts$)" "^core/markdown/(?!internal\\\\.ts$)" 'tools/architecture/architecture.test.ts' 'markdown-public-entry-only: accepts'
mutate 'micromark-import-fence' 'fe/eslint.config.js' "'micromark-*'," "'micromark-disabled-*'," 'tools/architecture/architecture.test.ts' 'markdown-micromark-import: accepts'
fi
mutate 'globalThis.fetch' 'fe/tools/architecture/no-core-platform-escape.mjs' "name === 'fetch'" "name === 'fetchDisabled'" 'tools/architecture/architecture-rules.test.ts' 'rejects globalThis.fetch'
mutate 'dynamic-import' 'fe/tools/architecture/no-core-platform-escape.mjs' "ImportExpression(/** @type {any} */ node) { context.report({ node, messageId: 'import' }); }" "ImportExpression(/** @type {any} */ node) { void node; }" 'tools/architecture/architecture-rules.test.ts' 'rejects dynamic import()'
