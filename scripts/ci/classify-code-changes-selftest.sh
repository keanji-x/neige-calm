#!/usr/bin/env bash
# Single-boundary fixtures for classify-code-changes.sh. Each red case has exactly one non-doc path.
set -euo pipefail

cd "$(dirname "$0")/../.."
classifier=scripts/ci/classify-code-changes.sh

check() {
  local expected="$1" label="$2"
  shift 2
  local actual
  actual="$(printf '%s\0' "$@" | "$classifier")"
  if [ "$actual" != "$expected" ]; then
    echo "classifier fixture failed: $label: expected $expected, got $actual" >&2
    exit 1
  fi
}

check false "docs tree" docs/design.md docs/oracle/contracts.yaml
check false "markdown outside docs" README.md fe/README.md
check false "legal prose" LICENSE NOTICE.txt
check true "non-README markdown beside code" fe/web/src/template.md
check true "one Rust source among docs" docs/design.md crates/calm-server/src/lib.rs
check true "one workflow among docs" docs/design.md .github/workflows/ci.yml
check true "one frontend source among docs" docs/design.md fe/web/src/main.tsx

empty="$("$classifier" < /dev/null)"
if [ "$empty" != true ]; then
  echo "classifier fixture failed: empty input must fail open to code=true, got $empty" >&2
  exit 1
fi

echo "classify-code-changes: fixtures passed"
