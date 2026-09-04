#!/usr/bin/env bash
# Surface-routing fixtures for classify-code-changes.sh plus the CI wiring that
# consumes its outputs.
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

check_mode() {
  local mode="$1" expected="$2" label="$3"
  shift 3
  local actual
  actual="$(printf '%s\0' "$@" | "$classifier" "$mode")"
  if [ "$actual" != "$expected" ]; then
    echo "$mode classifier fixture failed: $label: expected $expected, got $actual" >&2
    exit 1
  fi
}

# A Rust-test-only change cannot affect either frontend, generated API output,
# production-stack behavior, or frontend mutation evidence.
for mode in fe web openapi fe-e2e stack mutation; do
  check_mode "$mode" false "Rust test only" \
    crates/calm-server/tests/cases/migration_replay_harness.rs
done

# Production Rust can affect both integrated stacks and the API contract, but
# cannot change either frontend's pure unit/build graph or mutation manifest.
check_mode openapi true "Rust production API input" crates/calm-server/src/routes/version.rs
check_mode fe-e2e true "Rust production FE integration input" crates/calm-server/src/routes/version.rs
check_mode stack true "Rust production stack input" crates/calm-server/src/routes/version.rs
check_mode fe false "Rust production is not FE source" crates/calm-server/src/routes/version.rs
check_mode web false "Rust production is not legacy web source" crates/calm-server/src/routes/version.rs
check_mode mutation false "Rust production is not FE mutation input" crates/calm-server/src/routes/version.rs

# Database migrations affect production stacks but not generated HTTP types.
check_mode openapi false "database migration is not an API generator input" \
  crates/calm-truth/migrations/0086_rename_worker_flow_items_runtime_id.sql
check_mode fe-e2e true "database migration affects FE integration" \
  crates/calm-truth/migrations/0086_rename_worker_flow_items_runtime_id.sql
check_mode stack true "database migration affects stack integration" \
  crates/calm-truth/migrations/0086_rename_worker_flow_items_runtime_id.sql

# Maintained-frontend source owns its unit/browser/mutation and integrated FE
# evidence. Generic source does not affect the legacy bundle or stack wiring.
check_mode fe true "maintained frontend source" fe/web/src/main.tsx
check_mode mutation true "maintained frontend mutation input" fe/web/src/main.tsx
check_mode fe-e2e true "maintained frontend integration input" fe/web/src/main.tsx
check_mode web false "maintained frontend is not legacy web" fe/web/src/main.tsx
check_mode openapi false "maintained frontend is not API generator input" fe/web/src/main.tsx
check_mode stack false "maintained frontend source does not alter stack wiring" fe/web/src/main.tsx

# Frontend test edits still require the frontend and mutation suites, but do
# not change the production bundle exercised by Playwright or stack smoke.
check_mode fe true "maintained frontend test" fe/core/domain/track.test.ts
check_mode mutation true "maintained frontend test mutation witness" fe/core/domain/track.test.ts
check_mode fe-e2e false "maintained frontend test is not production input" \
  fe/core/domain/track.test.ts
check_mode stack false "maintained frontend test is not stack input" \
  fe/core/domain/track.test.ts

check_mode web true "legacy web source" web/src/api/client.ts
check_mode fe false "legacy web is not maintained frontend" web/src/api/client.ts
check_mode openapi false "legacy client source is not generated API output" web/src/api/client.ts
check_mode stack false "legacy web source does not alter stack wiring" web/src/api/client.ts

# These entry points control how production bundles are generated or served.
for path in fe/vite.config.ts fe/web/index.html web/vite.config.ts web/index.html \
  docker-compose.yml Makefile e2e/cases/010-stack-smoke.sh; do
  check_mode stack true "stack/build entry point $path" "$path"
done

# Generator dependencies and checked-in products must keep the drift gate on.
for path in web/package-lock.json web/src/api/generated.ts \
  fe/core/api/generated/openapi.json fe/core/api/generated/wire.ts; do
  check_mode openapi true "OpenAPI input/product $path" "$path"
done

# The legacy bundle imports this maintained-core key directly.
check_mode web true "legacy shared storage key" fe/core/keys/storage.ts

# Oracle docs are documentation to ordinary code jobs but executable mutation
# catalog input, as promised by classify-code-changes.sh's contract.
check_mode mutation true "oracle catalog" docs/oracle/SCHEMA.md
for mode in fe web openapi fe-e2e stack; do
  check_mode "$mode" false "ordinary documentation" docs/design.md
done
check_mode fe true "one FE source among Rust tests" \
  crates/calm-server/tests/version.rs fe/web/src/main.tsx
check_mode stack true "one stack input among FE tests" \
  fe/core/domain/track.test.ts docker/nginx.conf
check_mode mutation true "one oracle input among ordinary docs" \
  docs/design.md docs/oracle/SCHEMA.md

# CI authority changes and new, unclassified paths fail open to every surface.
for mode in rust fe web openapi fe-e2e stack mutation; do
  check_mode "$mode" true "CI workflow fail-open" .github/workflows/ci.yml
  check_mode "$mode" true "unknown path fail-open" tools/new-ci-input.dat
done

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

for mode in fe web openapi fe-e2e stack mutation; do
  empty_mode="$("$classifier" "$mode" < /dev/null)"
  if [ "$empty_mode" != true ]; then
    echo "$mode classifier fixture failed: empty input must fail open to true, got $empty_mode" >&2
    exit 1
  fi
done

ci_file=.github/workflows/ci.yml
require_ci_contract() {
  local needle="$1" label="$2"
  if ! grep -Fq -- "$needle" "$ci_file"; then
    echo "CI surface routing contract missing $label: $needle" >&2
    exit 1
  fi
}

for mode in fe web openapi fe-e2e stack mutation; do
  output_name="${mode//-/_}_changed"
  require_ci_contract \
    "$output_name: \${{ steps.classify.outputs.$output_name }}" \
    "$mode changes output"
  require_ci_contract \
    "needs.changes.outputs.$output_name == 'true'" \
    "$mode job routing"
done
require_ci_contract "RUN_RUST_CHECKS:" "step-scoped Rust lint switch"
require_ci_contract "name: Select no mutation evidence for unrelated changes" \
  "mutation no-op plan"
require_ci_contract 'steps.noop.outputs.selected' "mutation no-op outputs"

echo "classify-code-changes: fixtures passed"
