#!/usr/bin/env bash
# Run the Rust gates the way CI runs them — in BOTH feature combinations.
#
# Why both: `.github/workflows/ci.yml` sets `RUSTFLAGS: -D warnings` globally,
# and its jobs do NOT all use the same features.
#
#   * `lint` and `rust (test)` build with `--features calm-server/codex-e2e`.
#   * `openapi drift`, `chromium e2e`, `fe e2e` and `stack e2e (tier 1)` build
#     with DEFAULT features (`cargo run --bin emit-openapi`,
#     `cargo build --release ...`).
#
# Running only the first set is not a proxy for CI. `cargo check --all-targets`
# is not either: it pulls in the `calm-server` dev-dependency self-loop, which
# turns `fixtures` on and feature-unifies it into the lib build — so a symbol
# reachable only under `fixtures` looks alive locally and is dead code in CI.
# That is exactly how #1147 S2 shipped a `-D dead-code` failure to five jobs
# while every local gate was green.
#
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
cargo run --quiet --manifest-path Cargo.toml --bin emit-openapi > /tmp/neige-openapi-check.json
diff -q /tmp/neige-openapi-check.json fe/core/api/generated/openapi.json
echo "openapi: no drift"

if [[ "${1:-}" == "--quick" ]]; then
  step "6/6 tests SKIPPED (--quick)"
  exit 0
fi

step "6/6 nextest (WITH features, mirrors the rust job)"
cargo nextest run --workspace --locked --features calm-server/codex-e2e --no-fail-fast
