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
  calm-server/codex-e2e,calm-server/fixtures --profile ci --test-threads 8

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
clippy --workspace --all-targets --features calm-server/codex-e2e,calm-server/fixtures -- -D warnings
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
  calm-server/codex-e2e,calm-server/fixtures --profile ci

self_hosted_capture="$temp_root/self-hosted.args"
PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$self_hosted_capture" \
  scripts/run-ci-rust-nextest.sh self-hosted >/dev/null
assert_argv "$self_hosted_capture" nextest run --workspace --locked --features \
  calm-server/codex-e2e,calm-server/fixtures --profile ci --test-threads 8

partition_capture="$temp_root/partition.args"
PATH="$stub_bin:$PATH" \
  NEIGE_CODEX_BIN=/must-not-reach-nextest \
  RUST_NEXTEST_CAPTURE="$partition_capture" \
  scripts/run-ci-rust-nextest.sh github-hosted --partition hash:2/3 >/dev/null
assert_argv "$partition_capture" nextest run --workspace --locked --features \
  calm-server/codex-e2e,calm-server/fixtures --profile ci --partition hash:2/3

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

# Exercise the real workflow run blocks, shared producer and guard main. Cargo
# is stubbed; no Rust compilation or private nextest format is reproduced here.
SELFTEST_STUB_BIN="$stub_bin" python3 - <<'PYTEST'
import contextlib
import importlib.util
import io
import itertools
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path.cwd()
workflow = Path('.github/workflows/ci.yml').read_text()
# Discover declared events so additional non-Rust triggers need no test list edit.
events = [*re.findall(r'^  ([a-z_]+):', workflow.split('\nconcurrency:', 1)[0], re.M), 'unknown']
jobs = dict(re.findall(r'^  ([\w-]+):\n(.*?)(?=^  [\w-]+:|\Z)', workflow, re.M | re.S))
spec = importlib.util.spec_from_file_location('guard', ROOT / 'scripts/ci/check-nextest-overrides.py')
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)
source = ['--workspace', '--locked', '--features', 'calm-server/codex-e2e,calm-server/fixtures']
archive = ['--archive-file', 'nextest-archive.tar.zst']
protocol = json.dumps({'rust-suites': {'sample': {
    'package-name': 'sample', 'binary-id': 'sample', 'status': 'listed',
    'testcases': {'example': {'ignored': False, 'filter-match': {'status': 'matches'}}}}}})


def steps(job):
    body = re.sub(r'^      #.*\n', '', jobs[job], flags=re.M)
    return re.split(r'^      - ', body, flags=re.M)[1:]


def field(text, key, indent=8):
    match = re.search(r'^' + ' ' * indent + re.escape(key) + r': (.*)$', text, re.M)
    if not match:
        return None
    if match[1] != '|':
        return match[1]
    lines = text[match.end():].splitlines()[1:]
    return ''.join(line[indent + 2:] + '\n' for line in
                   itertools.takewhile(lambda line: not line.strip() or
                                       line.startswith(' ' * (indent + 2)), lines))


def expression(text, values):
    text = text.removeprefix('${{').removesuffix('}}').strip()
    text = re.sub(r'\b(?:github|needs|env|matrix)\.[\w.-]+',
                  lambda m: repr(values[m[0]]), text)
    text = text.replace('always()', 'True').replace('&&', ' and ').replace('||', ' or ')
    return eval(text, {'__builtins__': {}}, {})


def render(text, values):
    return re.sub(r'\$\{\{(.*?)\}\}', lambda m: str(expression(m[1], values)), text)


class ProducerTests(unittest.TestCase):
    def execute(self, event, job='rust-build', *, fail='', zero=False, command=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / '.config').mkdir()
            (root / 'scripts/ci').mkdir(parents=True)
            shutil.copy(ROOT / 'scripts/ci/prepare-rust-nextest.sh', root / 'scripts/ci')
            (root / '.config/nextest.toml').write_text(
                '[profile.ci]\n' if zero else
                '[[profile.renamed.overrides]]\nfilter = "all()"\n')
            log = root / 'calls'
            # The Python stub delegates to the real guard main with the fixture
            # config. Queries still go through the real guard subprocess path.
            stub = f'''#!{sys.executable}
import importlib.util, json, os, pathlib, sys
sys.dont_write_bytecode = True
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as out:
    out.write(json.dumps([name, *args]) + '\\n')
assert 'NEIGE_CODEX_BIN' not in os.environ or args[0] == 'build'
phase = ('guard' if name == 'python3' else
         'shim' if args[0] == 'build' else
         'build' if args[1] == 'archive' or 'binaries-only' in args else 'query')
if os.environ['FAIL'] == phase:
    sys.exit(42)
if name == 'python3':
    spec = importlib.util.spec_from_file_location('guard', {str(spec.origin)!r})
    guard = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(guard)
    guard.ROOT = pathlib.Path({tmp!r})
    sys.argv = sys.argv[1:]
    sys.exit(guard.main())
if name == 'cargo':
    print({protocol!r})
'''
            for name in ('cargo', 'python3'):
                path = root / name
                path.write_text(stub)
                path.chmod(0o755)
            env = dict(os.environ, PATH=f'{tmp}:' + os.environ['PATH'],
                       CALLS=str(log), FAIL=fail, GITHUB_EVENT_NAME=event,
                       NEIGE_CODEX_BIN='/must-not-reach-nextest')
            rc, uploads = 0, []
            selected = [s for s in steps(job) if
                        'scripts/ci/prepare-rust-nextest.sh' in s or
                        s.startswith('name: build cross-package') or
                        'uses: actions/upload-artifact@v7' in s]
            if command is not None:
                selected = ['name: direct\n        run: ' + command]
            for step in selected:
                condition = field(step, 'if')
                if condition and not expression(condition, {'github.event_name': event}):
                    continue
                self.assertNotIn('continue-on-error:', step)
                if 'uses: actions/upload-artifact@v7' in step:
                    self.assertEqual(field(step, 'if'),
                                     "github.event_name == 'pull_request'" if job == 'rust-build' else None)
                    self.assertEqual(field(step, 'archive', 10), 'false')
                    self.assertEqual(field(step, 'retention-days', 10), '1')
                    self.assertEqual(field(step, 'if-no-files-found', 10), 'error')
                    uploads.append(field(step, 'path', 10))
                    continue
                result = subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', field(step, 'run')],
                                        cwd=root, env=env, capture_output=True, text=True)
                rc = result.returncode
                if rc:
                    break
            calls = [json.loads(row) for row in log.read_text().splitlines()] if log.exists() else []
            return rc, calls, uploads

    def test_complete_producer_inputs(self):
        for event, job, filename in (
                ('pull_request', 'rust-build', 'nextest-archive.tar.zst'),
                ('push', 'rust-build', None),
                ('push', 'rust-main', 'nextest-main-archive.tar.zst')):
            for zero in (False, True):
                with self.subTest(event=event, job=job, zero=zero):
                    rc, calls, uploads = self.execute(event, job, zero=zero)
                    self.assertEqual(rc, 0)
                    if job == 'rust-main':
                        self.assertEqual(calls.pop(0), ['cargo', 'build', '--locked', '-p',
                                         'neige-mcp-stdio-shim', '--bin', 'neige-mcp-stdio-shim'])
                    input_args = ['--archive-file', filename] if filename else []
                    build = (['cargo', 'nextest', 'archive', *source, '--profile', 'ci', *input_args, '--timings']
                             if filename else ['cargo', 'nextest', 'list', *source, '--profile', 'ci',
                                               '--list-type', 'binaries-only', '--message-format', 'json'])
                    expected = [build, ['python3', 'scripts/ci/check-nextest-overrides.py', *input_args]]
                    if not zero:
                        query_source = [*input_args, '--workspace-remap', unittest.mock.ANY] if filename else source
                        expected.append(['cargo', 'nextest', 'list', *query_source,
                                         '--profile', 'renamed', '--message-format', 'json', '-E', 'all()'])
                    self.assertEqual(calls, expected)
                    self.assertEqual(uploads, [filename] if filename else [])

    def test_producer_failure_blocks_upload(self):
        for event, job in (('pull_request', 'rust-build'), ('push', 'rust-build'), ('push', 'rust-main')):
            for phase in ('build', 'guard'):
                with self.subTest(event=event, job=job, phase=phase):
                    rc, calls, uploads = self.execute(event, job, fail=phase)
                    self.assertEqual(rc, 42)
                    self.assertEqual(uploads, [])
                    guards = [c for c in calls if c[0] == 'python3']
                    self.assertEqual(len(guards), int(phase == 'guard'))
                    self.assertEqual(len(calls), 1 + int(job == 'rust-main') + int(phase == 'guard'))
        rc, calls, uploads = self.execute('push', 'rust-main', fail='shim')
        self.assertEqual((rc, len(calls), uploads), (42, 1, []))

    def test_unknown_event_and_modes(self):
        rc, calls, uploads = self.execute('unknown')
        self.assertNotEqual(rc, 0)
        self.assertEqual((calls, uploads), ([], []))
        for args in ('', 'unknown', 'archive', 'archive --archive-file',
                     'archive --archive-file ""', 'archive --other file',
                     'archive --archive-file file extra', 'inventory extra'):
            with self.subTest(args=args):
                rc, calls, uploads = self.execute('push', command='scripts/ci/prepare-rust-nextest.sh ' + args)
                self.assertEqual(rc, 2)
                self.assertEqual((calls, uploads), ([], []))

    def test_shared_guard_boundaries(self):
        for job in ('rust-build', 'rust-main'):
            prepare = [s for s in steps(job) if 'scripts/ci/prepare-rust-nextest.sh' in s]
            self.assertTrue(prepare)
            for step in prepare:
                self.assertIsNone(field(step, 'if'))
                self.assertNotIn('continue-on-error:', step)
            self.assertNotIn('matrix:', jobs[job])
        for path in ('scripts/run-rust-nextest.sh', 'scripts/run-ci-rust-nextest.sh'):
            self.assertNotIn('check-nextest-overrides', Path(path).read_text())
        self.assertNotIn('migration replay gate (#679 PR0-D)', workflow)


class RoutingTests(unittest.TestCase):
    def values(self, event, build='success', main='success', shards='success',
               changes='success', rust='true'):
        return {'github.event_name': event, 'needs.rust-build.result': build,
                'needs.rust-main.result': main, 'needs.rust-shards.result': shards,
                'needs.changes.result': changes, 'needs.changes.outputs.rust_changed': rust}

    def test_shards_select_event_owner(self):
        self.assertEqual(field(jobs['rust-shards'], 'needs', 4), '[changes, rust-build, rust-main]')
        condition = field(jobs['rust-shards'], 'if', 4)
        self.assertIn('always()', condition)
        for event, build in itertools.product(
                events,
                ('success', 'failure', 'cancelled', 'skipped')):
            # The two owners are independent: a skipped main must not block PR,
            # and failed inventory warming must not suppress push test coverage.
            for main in ('success', 'failure', 'cancelled', 'skipped'):
                with self.subTest(event=event, build=build, main=main):
                    expected = ((event == 'pull_request' and build == 'success') or
                                (event == 'push' and main == 'success'))
                    self.assertEqual(bool(expression(condition, self.values(event, build, main))), expected)

    def test_current_run_artifact_and_archive_execution(self):
        shard_steps = steps('rust-shards')
        download = next(s for s in shard_steps if s.startswith('uses: actions/download-artifact@'))
        # No cross-run, pattern, merge or fallback selection; download defaults to this run.
        self.assertEqual(download.strip(), 'uses: actions/download-artifact@v8\n'
                         '        with:\n          name: ${{ env.NEXTEST_ARCHIVE }}')
        run = next(s for s in shard_steps if 'id: rust_nextest' in s)
        for job in ('rust-build', 'rust-main', 'rust-shards'):
            checkout = next(s for s in steps(job) if s.startswith('uses: actions/checkout@'))
            self.assertEqual(checkout.strip(), 'uses: actions/checkout@v4\n'
                             '        with:\n          persist-credentials: false')
        for event, filename in (('pull_request', 'nextest-archive.tar.zst'),
                                ('push', 'nextest-main-archive.tar.zst')):
            values = self.values(event)
            values['env.NEXTEST_ARCHIVE'] = expression(field(jobs['rust-shards'], 'NEXTEST_ARCHIVE', 6), values)
            self.assertEqual(render(field(download, 'name', 10), values), filename)
            shards = json.loads(field(jobs['rust-shards'], 'shard', 8))
            for shard in shards:
                values['matrix.shard'] = shard
                with tempfile.TemporaryDirectory() as tmp:
                    capture = Path(tmp) / 'args'
                    # Existing production wrapper validates the file, unsets the
                    # provider binary and adds remap/extraction, without building.
                    archive_path = Path(tmp) / filename
                    archive_path.touch()
                    script = render(field(run, 'run'), values)
                    env = dict(os.environ, PATH=str(Path(os.environ['SELFTEST_STUB_BIN'])) + ':' + os.environ['PATH'],
                               NEXTEST_ARCHIVE=str(archive_path), RUST_NEXTEST_CAPTURE=str(capture),
                               NEIGE_CODEX_BIN='must-not-reach-nextest')
                    subprocess.run(['bash', '-e', '-c', script], cwd=ROOT, env=env, check=True)
                    actual = capture.read_bytes().decode().strip('\0').split('\0')
                    self.assertEqual(actual, ['nextest', 'run', '--archive-file', str(archive_path),
                                     '--workspace-remap', '.', '--extract-to', '.', '--extract-overwrite',
                                     '--profile', 'ci', '--partition', f'count:{shard}/{len(shards)}'])
        self.assertNotIn('rust-cache', jobs['rust-shards'])

    def test_required_failure_propagation(self):
        self.assertIn('always()', field(jobs['rust'], 'if', 4))
        self.assertEqual(set(re.findall(r'[\w-]+', field(jobs['rust'], 'needs', 4))),
                         {'changes', 'rust-build', 'rust-main', 'rust-shards'})
        step = next(s for s in steps('rust') if s.startswith('name: require'))
        for event, build, main, shards in itertools.product(
                ('pull_request', 'push', 'unknown'),
                ('success', 'failure', 'cancelled', 'skipped'),
                ('success', 'failure', 'cancelled', 'skipped'),
                ('success', 'failure', 'cancelled', 'skipped')):
            with self.subTest(event=event, build=build, main=main, shards=shards):
                values = self.values(event, build, main, shards)
                env = dict(os.environ, GITHUB_EVENT_NAME=event)
                for key, value in re.findall(r'^          (\w+): (.+)$', step, re.M):
                    env[key] = render(value, values)
                rc = subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', field(step, 'run')],
                                    env=env, capture_output=True).returncode
                expected = (event in ('pull_request', 'push') and build == shards == 'success'
                            and (event == 'pull_request' or main == 'success'))
                self.assertEqual(rc == 0, expected)

    def test_classification_and_non_rust_boundaries(self):
        for event, changes, rust in itertools.product(
                events,
                ('success', 'failure', 'cancelled', 'skipped'), ('true', 'false', '')):
            for job in ('rust-build', 'rust-main', 'rust'):
                with self.subTest(event=event, changes=changes, rust=rust, job=job):
                    expected = (event in ('pull_request', 'push') and
                                (job != 'rust-main' or event == 'push') and
                                (changes != 'success' or rust == 'true'))
                    self.assertEqual(bool(expression(field(jobs[job], 'if', 4),
                                     self.values(event, changes=changes, rust=rust))), expected)


class GuardTests(unittest.TestCase):
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
