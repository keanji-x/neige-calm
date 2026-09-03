#!/usr/bin/env bash
# Executes the real local and CI dispatch paths with a stub cargo, pinning the
# nextest environment and argv without compiling the workspace.
set -euo pipefail

cd "$(dirname "$0")/../.."
temp_root="$(mktemp -d)"
trap 'rm -rf -- "$temp_root"' EXIT

stub_bin="$temp_root/bin"
mkdir -p "$stub_bin"

cat >"$stub_bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = run ]; then
  cat "$LOCAL_RUST_GATES_SPEC"
elif [ "${1:-}" = nextest ]; then
  if [ -n "${NEIGE_CODEX_BIN+x}" ]; then
    echo "Rust gate leaked NEIGE_CODEX_BIN into nextest" >&2
    exit 1
  fi
  printf '%s\0' "$@" >"$RUST_NEXTEST_CAPTURE"
fi
EOF
chmod +x "$stub_bin/cargo"

assert_argv() {
  local capture="$1"
  shift
  local expected="$temp_root/expected.args"
  printf '%s\0' "$@" >"$expected"
  if ! cmp -s "$expected" "$capture"; then
    echo "Rust nextest argv mismatch (hex: expected, actual)" >&2
    od -An -tx1 "$expected" >&2
    od -An -tx1 "$capture" >&2
    exit 1
  fi
}

local_capture="$temp_root/local.args"
PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$local_capture" \
  LOCAL_RUST_GATES_SPEC="$PWD/fe/core/api/generated/openapi.json" \
  scripts/local-rust-gates.sh >/dev/null
assert_argv "$local_capture" nextest run --workspace --locked --features \
  calm-server/codex-e2e --profile ci --test-threads 8

hosted_capture="$temp_root/hosted.args"
PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$hosted_capture" \
  scripts/run-ci-rust-nextest.sh github-hosted >/dev/null
assert_argv "$hosted_capture" nextest run --workspace --locked --features \
  calm-server/codex-e2e --profile ci

self_hosted_capture="$temp_root/self-hosted.args"
PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$self_hosted_capture" \
  scripts/run-ci-rust-nextest.sh self-hosted >/dev/null
assert_argv "$self_hosted_capture" nextest run --workspace --locked --features \
  calm-server/codex-e2e --profile ci --test-threads 4

invalid_output=""
invalid_rc=0
invalid_output="$(scripts/run-rust-nextest.sh --test-threads 00 2>&1)" || invalid_rc=$?
if [ "$invalid_rc" -ne 2 ] || [ "$invalid_output" != "--test-threads requires a positive integer" ]; then
  echo "Rust nextest wrapper accepted a non-positive thread cap" >&2
  exit 1
fi

dispatch_output=""
dispatch_rc=0
dispatch_output="$(PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$temp_root/trailing.args" \
  scripts/run-ci-rust-nextest.sh github-hosted extra 2>&1)" || dispatch_rc=$?
dispatch_usage='usage: scripts/run-ci-rust-nextest.sh {github-hosted|self-hosted}'
if [ "$dispatch_rc" -ne 2 ] || [ "$dispatch_output" != "$dispatch_usage" ]; then
  echo "CI Rust nextest dispatch accepted trailing arguments" >&2
  exit 1
fi

ci_file=.github/workflows/ci.yml
ci_call='        run: scripts/run-ci-rust-nextest.sh "${{ runner.environment }}"'
grep_rc=0
ci_call_count="$(grep -Fxc "$ci_call" "$ci_file")" || grep_rc=$?
if [ "$grep_rc" -gt 1 ]; then
  echo "could not inspect CI Rust nextest wiring" >&2
  exit 1
fi
if [ "$ci_call_count" -ne 1 ]; then
  echo "CI must invoke the shared Rust nextest dispatch exactly once" >&2
  exit 1
fi

echo "local Rust gate safety selftest: passed"
