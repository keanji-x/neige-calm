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
  test "${2:-}" = run
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

# Execute the producer's real run blocks; do not duplicate its event routing.
python3 - <<'PYTEST'
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path.cwd()
workflow = Path('.github/workflows/ci.yml').read_text()
producer = workflow.split('  rust-build:\n', 1)[1].split('\n  rust-shards:', 1)[0]
steps = re.split(r'^      - name: ', producer, flags=re.M)[1:]
selected = [s for s in steps if s.startswith(('cargo nextest ', 'build nextest binary inventory', 'check nextest override matches'))]
spec = importlib.util.spec_from_file_location('guard', ROOT / 'scripts/ci/check-nextest-overrides.py')
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)
source = ['--workspace', '--locked', '--features', 'calm-server/codex-e2e']
archive = ['--archive-file', 'nextest-archive.tar.zst']
protocol = json.dumps({'rust-suites': {'sample': {
    'package-name': 'sample', 'binary-id': 'sample', 'status': 'listed',
    'testcases': {'example': {'ignored': False, 'filter-match': {'status': 'matches'}}}}}})


class ProducerTests(unittest.TestCase):
    def execute(self, event, *, fail='', zero=False):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / '.config').mkdir()
            (root / '.config/nextest.toml').write_text(
                '[profile.ci]\n' if zero else
                '[[profile.renamed.overrides]]\nfilter = "all()"\n')
            log = root / 'calls'
            # All production shell commands are observed with exact argv. The
            # python3 stub delegates to the real guard main, including zero rows.
            stub = f'''#!{sys.executable}
import importlib.util, json, os, pathlib, sys
sys.dont_write_bytecode = True
name = pathlib.Path(sys.argv[0]).name
with open(os.environ['CALLS'], 'a') as out:
    out.write(json.dumps([name, *sys.argv[1:]]) + '\\n')
if os.environ['FAIL'] == name:
    sys.exit(42)
if name == 'python3':
    assert 'NEIGE_CODEX_BIN' not in os.environ
    spec = importlib.util.spec_from_file_location('guard', {str(spec.origin)!r})
    guard = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(guard)
    guard.ROOT = pathlib.Path({tmp!r})
    sys.argv = sys.argv[1:]
    sys.exit(guard.main())
if name == 'cargo':
    print({protocol!r})
'''
            for name in ('cargo', 'python3', 'ls'):
                path = root / name
                path.write_text(stub)
                path.chmod(0o755)
            env = dict(os.environ, PATH=f'{tmp}:' + os.environ['PATH'],
                       CALLS=str(log), FAIL=fail, GITHUB_EVENT_NAME=event,
                       NEIGE_CODEX_BIN='/must-not-reach-guard')
            rc = 0
            for step in selected:
                condition = re.search(r'^        if: (.+)$', step, re.M)
                if condition:
                    match = re.fullmatch(r"github.event_name == '(pull_request|push)'", condition[1])
                    self.assertIsNotNone(match, condition[1])
                    if event != match[1]:
                        continue
                self.assertNotIn('continue-on-error:', step)
                run = re.search(r'^        run: (.+(?:\n.*)*)', step, re.M)[1]
                script = (''.join(line[10:] + '\n' for line in run.splitlines()[1:])
                          if run.startswith('|') else run)
                result = subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', script],
                                        cwd=root, env=env, capture_output=True, text=True)
                rc = result.returncode
                if rc:
                    break
            calls = [json.loads(row) for row in log.read_text().splitlines()] if log.exists() else []
            return rc, calls

    def test_pr_archive(self):
        rc, calls = self.execute('pull_request')
        self.assertEqual(rc, 0)
        self.assertEqual(calls[:3], [
            ['cargo', 'nextest', 'archive', *source, '--profile', 'ci', *archive],
            ['ls', '-l', 'nextest-archive.tar.zst'],
            ['python3', 'scripts/ci/check-nextest-overrides.py', *archive]])
        self.assertEqual(len(calls), 4)
        self.assertEqual(calls[3][:4], ['cargo', 'nextest', 'list', '--archive-file'])

    def test_push_inventory(self):
        rc, calls = self.execute('push')
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [
            ['cargo', 'nextest', 'list', *source, '--profile', 'ci', '--list-type', 'binaries-only'],
            ['python3', 'scripts/ci/check-nextest-overrides.py'],
            ['cargo', 'nextest', 'list', *source, '--profile', 'renamed',
             '--message-format', 'json', '-E', 'all()']])

    def test_push_zero_overrides(self):
        rc, calls = self.execute('push', zero=True)
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [
            ['cargo', 'nextest', 'list', *source, '--profile', 'ci', '--list-type', 'binaries-only'],
            ['python3', 'scripts/ci/check-nextest-overrides.py']])

    def test_build_failure(self):
        for event in ('pull_request', 'push'):
            with self.subTest(event=event):
                rc, calls = self.execute(event, fail='cargo')
                self.assertEqual(rc, 42)
                self.assertEqual(len(calls), 1, calls)

    def test_guard_failure(self):
        for event in ('pull_request', 'push'):
            with self.subTest(event=event):
                rc, calls = self.execute(event, fail='python3')
                self.assertEqual(rc, 42)
                self.assertEqual(calls[-1][0], 'python3')
                self.assertEqual(sum(c[0] == 'python3' for c in calls), 1)

    def test_unknown_event(self):
        rc, calls = self.execute('unknown')
        self.assertNotEqual(rc, 0)
        self.assertEqual(calls, [])

    def test_shared_guard_boundaries(self):
        self.assertEqual(workflow.count('scripts/ci/check-nextest-overrides.py'), 1)
        check = producer.index('      - name: check nextest override matches\n')
        upload = producer.index('      - name: upload nextest archive')
        self.assertLess(check, upload)
        step = producer[check:upload]
        self.assertEqual(step.count('      - name:'), 1)
        self.assertNotIn('if:', step)
        self.assertNotIn('continue-on-error:', step)
        self.assertNotIn('matrix:', producer)
        for path in ('scripts/run-rust-nextest.sh', 'scripts/run-ci-rust-nextest.sh'):
            self.assertNotIn('check-nextest-overrides', Path(path).read_text())

    def test_guard_main_contract(self):
        config = tomllib.loads((ROOT / '.config/nextest.toml').read_text())
        rows = [(profile, row['filter']) for profile, settings in config['profile'].items()
                for row in settings.get('overrides', [])]
        for args, source_args in (([], source), (archive, [*archive, '--workspace-remap', str(ROOT)])):
            with self.subTest(args=args), patch.object(sys, 'argv', ['guard', *args]), \
                    patch.dict(os.environ, NEIGE_CODEX_BIN='must-be-removed'), \
                    patch.object(guard.subprocess, 'run', return_value=
                                 subprocess.CompletedProcess([], 0, protocol)) as query, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(guard.main(), 0)
                self.assertEqual(query.call_count, len(rows))
                for call, (profile, expression) in zip(query.call_args_list, rows):
                    self.assertEqual(call.args[0], ['cargo', 'nextest', 'list', *source_args,
                                     '--profile', profile, '--message-format', 'json', '-E', expression])
                    self.assertEqual(call.kwargs['cwd'], ROOT)
                    self.assertIs(call.kwargs['check'], True)
                    self.assertIs(call.kwargs['text'], True)
                    self.assertEqual(call.kwargs['stdout'], subprocess.PIPE)
                    self.assertNotIn('NEIGE_CODEX_BIN', call.kwargs['env'])


unittest.main(verbosity=2)
PYTEST

echo "local Rust gate safety selftest: passed"
