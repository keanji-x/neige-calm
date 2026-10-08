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

if [ -n "${RUST_GATE_COMMANDS:-}" ]; then
  printf '%s\n' "$*" >> "$RUST_GATE_COMMANDS"
fi
if [ "${1:-}" = run ]; then
  cat "$LOCAL_RUST_GATES_SPEC"
elif [ "${1:-}" = nextest ]; then
  if [ -n "${NEIGE_CODEX_BIN+x}" ]; then
    echo "Rust gate leaked NEIGE_CODEX_BIN into nextest" >&2
    exit 1
  fi
  if [ "${2:-}" = list ]; then
    if [ "${NEXTEST_STUB_GUARD_FAIL:-}" = 1 ]; then exit 42; fi
    if [[ " $* " == *" --partition "* || " $* " == *" --extract-to "* || " $* " == *" --extract-overwrite "* ]]; then
      echo 'guard must use unpartitioned private inventory' >&2
      exit 1
    fi
    printf '%s\n' "$*" >> "$RUST_NEXTEST_CAPTURE.guard"
    python3 scripts/ci/check-nextest-overrides-selftest.py --emit-fixture "$@"
  else
    test "$(wc -l < "$RUST_NEXTEST_CAPTURE.guard")" -eq 2
    printf '%s\0' "$@" >"$RUST_NEXTEST_CAPTURE"
  fi
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

# Exercise quick against a real git history and worktree, including untracked
# paths. The cargo stub records commands; the production classifier is unchanged.
fixture="$temp_root/quick"
mkdir -p "$fixture/scripts/ci" "$fixture/fe/core/api/generated" "$fixture/docs" \
  "$fixture/crates/calm-server/src" "$fixture/crates/calm-server/tests"
cp scripts/local-rust-gates.sh "$fixture/scripts/"
cp scripts/ci/classify-code-changes.sh "$fixture/scripts/ci/"
cp fe/core/api/generated/openapi.json "$fixture/fe/core/api/generated/"
git -C "$fixture" init -q
git -C "$fixture" config user.name 'Gate test'
git -C "$fixture" config user.email 'gate@example.invalid'
printf 'baseline\n' > "$fixture/docs/change.md"
printf 'baseline\n' > "$fixture/crates/calm-server/src/routes.rs"
git -C "$fixture" add .
git -C "$fixture" commit -qm baseline
fixture_base="$(git -C "$fixture" rev-parse HEAD)"
git -C "$fixture" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main
git -C "$fixture" update-ref refs/remotes/origin/main "$fixture_base"

assert_quick() {
  local name="$1" openapi="$2"
  shift 2
  local commands="$temp_root/$name.commands"
  local expected="$temp_root/$name.expected"
  : > "$commands"
  PATH="$stub_bin:$PATH" \
    RUST_GATE_COMMANDS="$commands" \
    LOCAL_RUST_GATES_SPEC="$fixture/fe/core/api/generated/openapi.json" \
    "$fixture/scripts/local-rust-gates.sh" --quick "$@" > "$temp_root/$name.output"
  cat > "$expected" <<'EOF'
fmt --all --check
clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings
EOF
  if [ "$openapi" = true ]; then
    echo 'run --quiet --manifest-path Cargo.toml --bin emit-openapi' >> "$expected"
  fi
  if ! cmp -s "$expected" "$commands"; then
    echo "quick command selection mismatch: $name" >&2
    diff -u "$expected" "$commands" >&2
    exit 1
  fi
}

assert_quick empty true
printf 'docs\n' >> "$fixture/docs/change.md"
assert_quick unstaged-docs false
printf 'test\n' > "$fixture/crates/calm-server/tests/new.rs"
assert_quick untracked-test false
printf 'schema\n' > "$fixture/crates/calm-server/src/new schema.rs"
assert_quick untracked-schema true
rm "$fixture/crates/calm-server/src/new schema.rs"
printf 'route\n' >> "$fixture/crates/calm-server/src/routes.rs"
git -C "$fixture" add crates/calm-server/src/routes.rs
assert_quick staged-route true
git -C "$fixture" commit -qm route
assert_quick committed-route true
assert_quick explicit-base false --base HEAD
assert_quick missing-base true --base refs/heads/nonexistent
# A rename out of a schema path must retain the deleted source path.
git -C "$fixture" mv crates/calm-server/src/routes.rs docs/old-route.md
assert_quick renamed-route true --base HEAD
# Failure and invalid arguments must not be mistaken for a green skipped gate.
if PATH="$stub_bin:$PATH" "$fixture/scripts/local-rust-gates.sh" --base >/dev/null 2>&1; then
  echo 'quick accepted a missing base argument' >&2
  exit 1
fi
if PATH="$stub_bin:$PATH" "$fixture/scripts/local-rust-gates.sh" --typo >/dev/null 2>&1; then
  echo 'quick accepted an unknown argument' >&2
  exit 1
fi

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
  calm-server/codex-e2e --profile ci --test-threads 8

partition_capture="$temp_root/partition.args"
PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$partition_capture" \
  scripts/run-ci-rust-nextest.sh github-hosted --partition hash:2/3 >/dev/null
assert_argv "$partition_capture" nextest run --workspace --locked --features \
  calm-server/codex-e2e --profile ci --partition hash:2/3

archive_capture="$temp_root/archive.args"
archive_file="$temp_root/tests.tar.zst"
touch "$archive_file"
PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$archive_capture" \
  scripts/run-ci-rust-nextest.sh github-hosted --archive-file "$archive_file" \
  --partition count:2/6 >/dev/null
assert_argv "$archive_capture" nextest run --archive-file "$archive_file" \
  --workspace-remap . --extract-to . --extract-overwrite --profile ci \
  --partition count:2/6

# The self-hosted archive route shares the inventory but retains its thread cap.
self_archive_capture="$temp_root/self-archive.args"
PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$self_archive_capture" \
  scripts/run-ci-rust-nextest.sh self-hosted --archive-file "$archive_file" \
  --partition hash:1/2 >/dev/null
assert_argv "$self_archive_capture" nextest run --archive-file "$archive_file" \
  --workspace-remap . --extract-to . --extract-overwrite --profile ci \
  --test-threads 8 --partition hash:1/2

# Every production route must stop before run when guard evaluation fails.
for route in github-hosted self-hosted; do
  for mode in live archive; do
    failure_capture="$temp_root/failure-$route-$mode.args"
    failure_args=(--partition hash:1/2)
    if [ "$mode" = archive ]; then failure_args+=(--archive-file "$archive_file"); fi
    if PATH="$stub_bin:$PATH" NEXTEST_STUB_GUARD_FAIL=1 \
      RUST_NEXTEST_CAPTURE="$failure_capture" \
      scripts/run-ci-rust-nextest.sh "$route" "${failure_args[@]}" >/dev/null 2>&1; then
      echo 'guard failure did not block run' >&2; exit 1
    fi
    test ! -e "$failure_capture"
  done
done
# Archive guard sees the same archive, but never run-only partition/extraction flags.
if ! grep -Fq -- "--archive-file $archive_file" "$archive_capture.guard"; then
  echo 'archive guard did not receive archive inventory' >&2; exit 1
fi

invalid_output=""
invalid_rc=0
invalid_output="$(scripts/run-rust-nextest.sh --test-threads 00 2>&1)" || invalid_rc=$?
if [ "$invalid_rc" -ne 2 ] || [ "$invalid_output" != "--test-threads requires a positive integer" ]; then
  echo "Rust nextest wrapper accepted a non-positive thread cap" >&2
  exit 1
fi

invalid_output=""
invalid_rc=0
invalid_output="$(scripts/run-rust-nextest.sh --partition count:5/4 2>&1)" || invalid_rc=$?
if [ "$invalid_rc" -ne 2 ] || [ "$invalid_output" != "--partition requires N <= M" ]; then
  echo "Rust nextest wrapper accepted a partition index above its total" >&2
  exit 1
fi

dispatch_output=""
dispatch_rc=0
dispatch_output="$(PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$temp_root/trailing.args" \
  scripts/run-ci-rust-nextest.sh github-hosted extra 2>&1)" || dispatch_rc=$?
dispatch_usage='usage: scripts/run-rust-nextest.sh [--archive-file FILE] [--test-threads N] [--partition KIND:N/M]'
if [ "$dispatch_rc" -ne 2 ] || [ "$dispatch_output" != "$dispatch_usage" ]; then
  echo "CI Rust nextest dispatch accepted trailing arguments" >&2
  exit 1
fi

ci_file=.github/workflows/ci.yml
ci_call='          scripts/run-ci-rust-nextest.sh \'
grep_rc=0
ci_call_count="$(grep -Fxc "$ci_call" "$ci_file")" || grep_rc=$?
if [ "$grep_rc" -gt 1 ]; then
  echo "could not inspect CI Rust nextest wiring" >&2
  exit 1
fi
# Once for the PR shards (from the archive), once for the hosted main push.
if [ "$ci_call_count" -ne 2 ]; then
  echo "CI must invoke the shared Rust nextest dispatch exactly twice" >&2
  exit 1
fi
if grep -Fq 'migration replay gate (#679 PR0-D)' "$ci_file"; then
  echo "CI must not rerun migration replay outside the full nextest suite" >&2
  exit 1
fi

python3 - "$local_capture" "$hosted_capture" "$self_hosted_capture" \
  "$partition_capture" "$archive_capture" "$self_archive_capture" "$archive_file" <<'PYTEST'
import pathlib
import sys
import tomllib
with open('.config/nextest.toml', 'rb') as stream:
    overrides = tomllib.load(stream)['profile']['ci']['overrides']
for capture in sys.argv[1:-1]:
    archive = capture in sys.argv[-3:-1]
    source = (['--archive-file', sys.argv[-1], '--workspace-remap', str(pathlib.Path.cwd())]
              if archive else ['--workspace', '--locked', '--features', 'calm-server/codex-e2e'])
    expected = [' '.join(['nextest', 'list', *source, '--profile', 'ci',
                          '--message-format', 'json', '-E', row['filter']]) for row in overrides]
    actual = pathlib.Path(capture + '.guard').read_text().splitlines()
    assert actual == expected, (capture, expected, actual)
PYTEST

echo "local Rust gate safety selftest: passed"
