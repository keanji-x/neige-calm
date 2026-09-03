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

check_rust() {
  local expected="$1" label="$2"
  shift 2
  local actual
  actual="$(printf '%s\0' "$@" | "$classifier" rust)"
  if [ "$actual" != "$expected" ]; then
    echo "Rust classifier fixture failed: $label: expected $expected, got $actual" >&2
    exit 1
  fi
}

check_rust false "frontend-only" fe/web/src/main.tsx web/src/main.tsx
check_rust false "documentation-only" docs/oracle/contracts.yaml README.md
check_rust true "crate source" crates/calm-server/src/lib.rs
check_rust true "workspace configuration" Cargo.lock .config/nextest.toml
check_rust true "compiled source outside crates" plugins/git-forge/main.rs
check_rust true "CI command changed" .github/workflows/ci.yml

empty="$("$classifier" < /dev/null)"
if [ "$empty" != true ]; then
  echo "classifier fixture failed: empty input must fail open to code=true, got $empty" >&2
  exit 1
fi

empty_rust="$("$classifier" rust < /dev/null)"
if [ "$empty_rust" != true ]; then
  echo "classifier fixture failed: empty input must fail open to rust=true, got $empty_rust" >&2
  exit 1
fi

echo "classify-code-changes: fixtures passed"
