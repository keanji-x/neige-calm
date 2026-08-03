#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
validator="$repo_root/fe/tools/oracle/validator.ts"
fixture="$repo_root/fe/tools/oracle/fixtures/former-id-unique/negative/data.yaml"
unsupported="$repo_root/docs/oracle/anchor-unsupported.yaml"
tmp_dir=$(mktemp -d)
cp "$validator" "$tmp_dir/validator.ts"
cp "$fixture" "$tmp_dir/former-id.yaml"
cp "$unsupported" "$tmp_dir/anchor-unsupported.yaml"
trap 'cp "$tmp_dir/validator.ts" "$validator"; cp "$tmp_dir/former-id.yaml" "$fixture"; cp "$tmp_dir/anchor-unsupported.yaml" "$unsupported"; rm -rf "$tmp_dir"' EXIT

replace() {
  local file=$1 old=$2 new=$3
  cp "$file" "$tmp_dir/before"
  OLD="$old" NEW="$new" node - "$file" <<'NODE'
const fs = require('node:fs');
const file = process.argv[2];
const text = fs.readFileSync(file, 'utf8');
if (!text.includes(process.env.OLD)) process.exit(11);
fs.writeFileSync(file, text.replaceAll(process.env.OLD, process.env.NEW));
NODE
  if cmp -s "$tmp_dir/before" "$file"; then
    echo "mutation did not change $file" >&2
    exit 12
  fi
}

run_mutation() {
  local name=$1
  set +e
  output=$(cd "$repo_root/fe" && npx vitest run 2>&1)
  code=$?
  set -e
  count=$(printf '%s\n' "$output" | sed -nE 's/.*Tests  ([0-9]+) failed.*/\1/p' | tail -1)
  printf '%s\texit=%s\tfailed=%s\n' "$name" "$code" "${count:-0}"
  printf '%s\n' "$output" | sed -nE 's/^ FAIL  .* > (.*)$/  - \1/p' | sort -u
  cp "$tmp_dir/validator.ts" "$validator"
  cp "$tmp_dir/former-id.yaml" "$fixture"
  cp "$tmp_dir/anchor-unsupported.yaml" "$unsupported"
  if [[ $code -eq 0 ]]; then exit 13; fi
}

replace "$validator" "if (anchor.error && anchor.subtype) actualBaseline.set(id, anchor.subtype);" "if (false && anchor.error && anchor.subtype) actualBaseline.set(id, anchor.subtype);"
run_mutation source-anchor-unconditional-accept

replace "$validator" "if (!isIdentifierCharacter(before) && !isIdentifierCharacter(after)) offsets.push(offset);" "if (true) offsets.push(offset);"
replace "$validator" "if (!isIdentifierCharacter(before) && !isIdentifierCharacter(after)) {" "if (true) {"
run_mutation identifier-boundary-disabled

replace "$validator" "ts.createScanner(ts.ScriptTarget.Latest, true," "ts.createScanner(ts.ScriptTarget.Latest, false,"
run_mutation comments-count-as-code

replace "$validator" "if (present.length === 0) return {" "if (false && present.length === 0) return {"
run_mutation missing-identifier-silent

replace "$validator" "if (currentIds.has(entry.former_id) || previous) {" "if (currentIds.has(entry.former_id)) {"
run_mutation duplicate-retired-handle-bypass

replace "$validator" "if (Object.hasOwn(entry, 'former_id')) {" "if (false && Object.hasOwn(entry, 'former_id')) {"
run_mutation former-id-unconditional-accept

replace "$validator" "if (baseline.get(id) !== subtype) add('<baseline>', id, 'source-anchor', \`unbaselined \${subtype}\`);" "if (false && baseline.get(id) !== subtype) add('<baseline>', id, 'source-anchor', \`unbaselined \${subtype}\`);"
run_mutation baseline-unbaselined-loop-disabled

replace "$validator" "if (actualBaseline.get(id) !== subtype) add('<baseline>', id, 'source-anchor', \`stale baseline \${subtype}\`);" "if (false && actualBaseline.get(id) !== subtype) add('<baseline>', id, 'source-anchor', \`stale baseline \${subtype}\`);"
run_mutation baseline-stale-loop-disabled

replace "$validator" "if (options.anchorBaselinePath" "if (false && options.anchorBaselinePath"
run_mutation baseline-count-guard-disabled

replace "$validator" "fields.push({ text: withoutCssComments(node.selector), startLine });" "fields.push({ text: node.selector, startLine });"
run_mutation css-selector-inline-comments-count-as-code

replace "$validator" "result.get(identifier)!.add(lineAtFieldOffset(field.startLine, field.text, offset));" "result.get(identifier)!.add(startLine);"
run_mutation css-field-match-offset-ignored

cp "$unsupported" "$tmp_dir/before"
(cd "$repo_root/fe" && node --input-type=module - "$unsupported" <<'NODE'
import { readFileSync, writeFileSync } from 'node:fs';
import { parse, stringify } from 'yaml';
const file = process.argv[2];
const rows = parse(readFileSync(file, 'utf8'));
rows.push(rows[0]);
writeFileSync(file, stringify(rows));
NODE
)
if cmp -s "$tmp_dir/before" "$unsupported"; then
  echo "mutation did not change $unsupported" >&2
  exit 12
fi
run_mutation duplicate-unsupported-id
