#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "$0")/../../.." && pwd)

mutate() {
  local target=$1 before=$2 after=$3 scratch
  scratch=$(mktemp)
  P0B_MUTATION_BEFORE="$before" P0B_MUTATION_AFTER="$after" \
    perl -0pe 's/\Q$ENV{"P0B_MUTATION_BEFORE"}\E/$ENV{"P0B_MUTATION_AFTER"}/ or die "mutation pattern absent\n"' \
    "$target" > "$scratch"
  if cmp -s "$target" "$scratch"; then
    echo "mutation made no change: $target" >&2
    exit 1
  fi
  mv "$scratch" "$target"
}

run_mutation() {
  local label=$1 target=$2 before=$3 after=$4 pattern=$5 backup
  backup=$(mktemp)
  cp "$target" "$backup"
  trap 'cp "$backup" "$target"; rm -f "$backup"' EXIT INT TERM
  mutate "$target" "$before" "$after"
  echo "mutation: $label"
  set +e
  (cd "$repo_dir/fe" && npx vitest run --project platform-independent \
    tools/test-tier/checker.test.ts tools/test-tier/vitest-projects.test.ts -t "$pattern")
  local status=$?
  set -e
  cp "$backup" "$target"
  rm -f "$backup"
  trap - EXIT INT TERM
  return "$status"
}

case "${1:-}" in
  browser-proof)
    (cd "$repo_dir/fe" && npm run test:browser -- --run tools/test-tier/layout.browser.test.ts)
    ;;
  jsdom-proof)
    proof="$repo_dir/fe/web/src/ui/layout-jsdom-proof.test.ts"
    report=$(mktemp)
    test ! -e "$proof"
    cp "$repo_dir/fe/tools/test-tier/layout.browser.test.ts" "$proof"
    trap 'rm -f "$proof" "$report"' EXIT
    if (cd "$repo_dir/fe" && npx vitest run --project web-dom web/src/ui/layout-jsdom-proof.test.ts) >"$report" 2>&1; then
      echo "jsdom proof unexpectedly passed" >&2
      exit 1
    fi
    grep -F 'expected +0 to be 37' "$report"
    cat "$report"
    ;;
  overlap-browser)
    run_mutation "$1" "$repo_dir/fe/vitest.config.ts" \
      "exclude: ['**/*.browser.test.{ts,tsx}', 'web/src/ui/**/*.test.{ts,tsx}']" \
      "exclude: ['web/src/ui/**/*.test.{ts,tsx}']" \
      "assigns representative|accepts the positive"
    ;;
  ignore-migrated)
    run_mutation "$1" "$repo_dir/fe/tools/test-tier/checker.ts" \
      "entry.migration !== 'migrated'" "entry.migration === 'migrated'" \
      "accepts the positive|rejects every negative|extra project|parses every"
    ;;
  ignore-tier)
    run_mutation "$1" "$repo_dir/fe/tools/test-tier/checker.ts" \
      "actual.length !== 1 || !expected.some((project) => project === actual[0])" "actual.length !== 1" \
      "rejects every negative|wrong tier even"
    ;;
  allow-overlap)
    run_mutation "$1" "$repo_dir/fe/tools/test-tier/checker.ts" \
      "actual.length !== 1 || !expected.some((project) => project === actual[0])" \
      "!expected.some((project) => project === actual[0])" \
      "extra project|overlap for jsdom"
    ;;
  *)
    echo "usage: $0 {browser-proof|jsdom-proof|overlap-browser|ignore-migrated|ignore-tier|allow-overlap}" >&2
    exit 2
    ;;
esac
