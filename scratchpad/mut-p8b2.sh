#!/usr/bin/env bash
set -euo pipefail

repo=$(git rev-parse --show-toplevel)
checker=(node "$repo/fe/tools/styles/repository-check.mjs")

mutate() {
  local label=$1 target=$2 mutated=$3 backup result
  backup=$(mktemp)
  cp "$target" "$backup"
  if cmp -s "$target" "$mutated"; then
    echo "$label: refused no-op mutation" >&2
    rm -f "$backup"
    return 2
  fi
  cp "$mutated" "$target"
  set +e
  (cd "$repo/fe" && "${checker[@]}")
  result=$?
  set -e
  cp "$backup" "$target"
  rm -f "$backup" "$mutated"
  if (( result == 0 )); then
    echo "$label: unexpectedly green" >&2
    return 1
  fi
  echo "$label: red (exit $result)"
}

mutate_tools_test() {
  local label=$1 target=$2 mutated=$3 backup result
  backup=$(mktemp)
  cp "$target" "$backup"
  if cmp -s "$target" "$mutated"; then
    echo "$label: refused no-op mutation" >&2
    rm -f "$backup"
    return 2
  fi
  cp "$mutated" "$target"
  set +e
  (cd "$repo/fe" && npx vitest run tools/styles/styles.test.ts)
  result=$?
  set -e
  cp "$backup" "$target"
  rm -f "$backup" "$mutated"
  if (( result == 0 )); then
    echo "$label: unexpectedly green" >&2
    return 1
  fi
  echo "$label: red (exit $result)"
}

mutate_ownership_test() {
  local label=$1 target=$2 mutated=$3 backup result
  backup=$(mktemp)
  cp "$target" "$backup"
  if cmp -s "$target" "$mutated"; then
    echo "$label: refused no-op mutation" >&2
    rm -f "$backup"
    return 2
  fi
  cp "$mutated" "$target"
  set +e
  (cd "$repo/fe" && OWNERSHIP_EVENT_NAME=push node tools/ownership/check-readonly-change-requests.mjs)
  result=$?
  set -e
  cp "$backup" "$target"
  rm -f "$backup" "$mutated"
  if (( result == 0 )); then
    echo "$label: unexpectedly green" >&2
    return 1
  fi
  echo "$label: red (exit $result)"
}

tokens="$repo/fe/web/src/styles/tokens.css"
entry="$repo/fe/web/src/styles/entry.css"
candidate=$(mktemp)
cp "$tokens" "$candidate"
printf '\nbutton { padding: 0; }\n' >> "$candidate"
mutate tokens-unlayered-button "$tokens" "$candidate"

candidate=$(mktemp)
sed 's/@import "\.\/tokens\.css" layer(tokens);/@import "\.\/tokens\.css";/' "$entry" > "$candidate"
mutate entry-import-without-layer "$entry" "$candidate"

candidate=$(mktemp)
sed 's/@layer reset, vendor, tokens, base, astryx, ui, features, overrides;/@layer overrides, features, ui, astryx, base, tokens, vendor, reset;/' "$entry" > "$candidate"
mutate reversed-production-order "$entry" "$candidate"

repository_check="$repo/fe/tools/styles/repository-check.mjs"
candidate=$(mktemp)
sed "s/specifier\.text\.split(\/\[?#\]\/, 1)\[0\]/specifier.text/" "$repository_check" > "$candidate"
mutate_tools_test css-query-fragment-pathname "$repository_check" "$candidate"

candidate=$(mktemp)
sed 's/const name = ts\.isComputedPropertyName(node\.name) ? node\.name\.expression : node\.name;/const name = node.name;/' "$repository_check" > "$candidate"
mutate_tools_test data-computed-static-name "$repository_check" "$candidate"

candidate=$(mktemp)
sed 's/|| expiryDate\.toISOString()\.slice(0, 10) !== entry\.expiry/|| false/' "$repository_check" > "$candidate"
mutate_tools_test expiry-calendar-normalization "$repository_check" "$candidate"

ownership_validator="$repo/fe/tools/ownership/validator.ts"
candidate=$(mktemp)
sed "/return execFileSync('git', \['merge-base', injectedBase, headRef\], {/,/}).trim();/c\\
        return injectedBase;" "$ownership_validator" > "$candidate"
mutate_ownership_test injected-base-without-merge-base "$ownership_validator" "$candidate"
