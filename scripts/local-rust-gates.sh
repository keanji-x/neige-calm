#!/usr/bin/env bash
# Run the Rust gates the way CI runs them — in BOTH feature combinations: `-D warnings`
# is global and CI jobs differ in features. `cargo check --all-targets` is no proxy: the
# dev-dependency self-loop turns `fixtures` on, so dead-in-CI code looks alive locally.
# Usage: scripts/local-rust-gates.sh [--quick]
#   --quick skips the full test run (keeps both compile matrices + openapi).
set -euo pipefail
cd "$(dirname "$0")/.."

export RUSTFLAGS="${RUSTFLAGS:--D warnings}"
export RUSTC_WRAPPER=""
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

step() { printf '\n=== %s\n' "$1"; }

step "1/6 fmt"
cargo fmt --all --check

step "2/6 clippy (WITH features, mirrors the lint job)"
cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings

step "3/6 lib check (DEFAULT features, mirrors openapi-drift + the e2e builds)"
cargo check -p calm-server --lib

step "4/6 release build (DEFAULT features, the exact e2e-job command)"
cargo build --release -p calm-server -p calm-codex-bridge -p neige-mcp-stdio-shim \
  -p calm-proc-supervisor --bin calm-server --bin neige-codex-bridge \
  --bin neige-mcp-stdio-shim --bin calm-proc-supervisor --locked

step "5/6 openapi drift (DEFAULT features)"
# The maintained frontend owns the OpenAPI and wire outputs; this Rust-only check compares the JSON spec.
cargo run --quiet --manifest-path Cargo.toml --bin emit-openapi > /tmp/neige-openapi-check.json
openapi_stale=0
for spec in fe/core/api/generated/openapi.json; do
  diff -q /tmp/neige-openapi-check.json "$spec" || openapi_stale=1
done
if [[ "$openapi_stale" == "1" ]]; then
  echo "openapi: STALE — regenerate with: (cd fe && npm run gen:api)" >&2
  echo "         then commit the generated wire types it also rewrites." >&2
  exit 1
fi
echo "openapi: no drift (maintained spec; generated .ts still needs npm run gen:api)"

if [[ "${1:-}" == "--quick" ]]; then
  step "6/6 tests SKIPPED (--quick)"
  exit 0
fi

step "6/6 nextest (WITH features, mirrors the rust job)"
# Local runs share this production host with live services. The shared wrapper
# pins the CI profile and disables real Codex; this local cap prevents flakes.
scripts/run-rust-nextest.sh --test-threads 8
