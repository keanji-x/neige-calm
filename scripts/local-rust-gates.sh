#!/usr/bin/env bash
# Quick preflight compiles one feature configuration; CI owns the default-feature
# and release checks. The full mode retains both compile matrices and nextest.
# Usage: scripts/local-rust-gates.sh [--quick] [--base REF]
#   --quick runs fmt, feature-enabled clippy, and OpenAPI only for relevant paths.
#   --base compares against the merge-base with REF (default: origin/HEAD).
# Missing comparison refs, failed path reads and empty diffs keep OpenAPI enabled.
set -euo pipefail
cd "$(dirname "$0")/.."

quick=false
base_ref=origin/HEAD
while (($#)); do
  case "$1" in
    --quick) quick=true; shift ;;
    --base)
      if (($# < 2)) || [[ -z "$2" ]]; then
        echo "--base requires a git ref" >&2
        exit 2
      fi
      base_ref="$2"; shift 2 ;;
    *) echo "usage: scripts/local-rust-gates.sh [--quick] [--base REF]" >&2; exit 2 ;;
  esac
done

scratch="$(mktemp -d)"
trap 'rm -rf -- "$scratch"' EXIT
openapi_changed=true
if [[ "$quick" == true ]]; then
  if base="$(git merge-base HEAD "$base_ref" 2>/dev/null)" \
    && git diff --no-renames --name-only -z "$base" -- > "$scratch/paths" \
    && git ls-files --others --exclude-standard -z >> "$scratch/paths"; then
    openapi_changed="$(scripts/ci/classify-code-changes.sh openapi < "$scratch/paths")"
  else
    echo "openapi: comparison unavailable; checking conservatively" >&2
  fi
fi

export RUSTFLAGS="${RUSTFLAGS:--D warnings}"
export RUSTC_WRAPPER=""
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

step() { printf '\n=== %s\n' "$1"; }

step "fmt"
cargo fmt --all --check

step "clippy (WITH features, mirrors the lint job)"
cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings

if [[ "$quick" == false ]]; then
  step "lib check (DEFAULT features, mirrors openapi-drift + the e2e builds)"
  cargo check -p calm-server --lib

  step "release build (DEFAULT features, the exact e2e-job command)"
  cargo build --release -p calm-server -p calm-codex-bridge -p neige-mcp-stdio-shim \
    -p calm-proc-supervisor --bin calm-server --bin neige-codex-bridge \
    --bin neige-mcp-stdio-shim --bin calm-proc-supervisor --locked
fi

if [[ "$openapi_changed" == true ]]; then
  step "openapi drift (DEFAULT features)"
  # The maintained frontend owns the OpenAPI and wire outputs; this Rust-only check compares the JSON spec.
  openapi_check="$scratch/openapi.json"
  cargo run --quiet --manifest-path Cargo.toml --bin emit-openapi > "$openapi_check"
  openapi_stale=0
  for spec in fe/core/api/generated/openapi.json; do
    diff -q "$openapi_check" "$spec" || openapi_stale=1
  done
  if [[ "$openapi_stale" == "1" ]]; then
    echo "openapi: STALE — regenerate with: (cd fe && npm run gen:api)" >&2
    echo "         then commit the generated wire types it also rewrites." >&2
    exit 1
  fi
  echo "openapi: no drift (maintained spec; generated .ts still needs npm run gen:api)"

else
  echo "openapi: skipped (no relevant changed paths)"
fi

if [[ "$quick" == true ]]; then
  step "tests SKIPPED (--quick)"
  exit 0
fi

step "nextest (WITH features, mirrors the rust job)"
# Local runs share this production host with live services. The shared wrapper
# pins the CI profile and disables real Codex; this local cap prevents flakes.
scripts/run-rust-nextest.sh --test-threads 8
