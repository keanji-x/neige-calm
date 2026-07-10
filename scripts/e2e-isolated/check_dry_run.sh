#!/usr/bin/env bash
# #863 — dry-run golden assertions for the docker-isolated codex-e2e tier.
# Runs run.sh --dry-run with a fully fake environment (needs NO docker daemon,
# NO cargo, NO codex install) and asserts the security-critical shape of the
# produced `docker run` argv. Wired as `make e2e-codex-isolated-check`.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

OUT="$(
    CARGO_TARGET_DIR=/fake/target \
    NEIGE_CODEX_BIN=/fake/codex/bin/codex \
    CALM_HOST_PROXY_HOST=127.0.0.1 \
    CALM_HOST_PROXY_PORT=2080 \
    DECOYS=0 \
    "$SCRIPT_DIR/run.sh" --dry-run \
        --test-bin /fake/target/debug/deps/codex_forge_e2e-cafebabe \
        --test some_test_name 2>&1
)"

# The argv under test is only the run-container block (the forwarder container
# legitimately uses --network host and is not part of this argv).
ARGV="$(printf '%s\n' "$OUT" \
    | sed -n '/--- dry-run: docker run argv (run container) ---/,/--- dry-run: end argv ---/p')"
[ -n "$ARGV" ] || { echo "FAIL: dry-run did not print a delimited docker run argv"; printf '%s\n' "$OUT"; exit 1; }

fail=0
must_contain() {
    if ! printf '%s' "$ARGV" | grep -qF -- "$1"; then
        echo "FAIL: argv missing required token: $1"
        fail=1
    fi
}
must_not_contain() {
    if printf '%s' "$ARGV" | grep -qF -- "$1"; then
        echo "FAIL: argv contains forbidden token: $1"
        fail=1
    fi
}

# -- required rails (design §B/§C/§E golden list) --
must_contain '--network none'
must_contain '--pids-limit=6000'
must_contain '--memory=24g'
must_contain '--memory-swap=24g'
must_contain '--cpus=8'
must_contain '--user '
must_contain 'seccomp=unconfined'
must_contain 'apparmor=unconfined'
must_contain '--init'
must_contain '--rm'
must_contain '/fake/target:/fake/target:ro'          # target dir ro mount
must_contain '/fake/codex/bin/codex:/opt/codex/codex:ro'
must_contain '/.codex/auth.json:ro'                  # single-file auth mount
must_contain 'NEIGE_CODEX_PROXY=http://127.0.0.1:2081'
must_contain 'NEIGE_CODEX_BIN=/opt/codex/codex'
must_contain '--tmpfs'

# repo mounted ro at its identical host path
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
must_contain "$REPO_ROOT:$REPO_ROOT:ro"

# -- forbidden: anything that would widen the blast radius --
must_not_contain '--publish'
must_not_contain ' -p '
must_not_contain '--cap-add'
must_not_contain 'docker.sock'
must_not_contain '--pid '                            # (substring-safe: does not match --pids-limit)
must_not_contain '--pid='
must_not_contain '--privileged'
must_not_contain '--network host'
must_not_contain '--network bridge'

# dry-run must never execute docker: the printed argv line is the only place
# the word `docker` may appear, and it must be the golden print, not output
# of a real invocation. Cheap proxy: run.sh exited 0 with fake paths above,
# which is impossible if it had actually invoked docker/cargo on them.

if [ "$fail" -ne 0 ]; then
    echo "--- captured argv block ---"
    printf '%s\n' "$ARGV"
    exit 1
fi
echo "e2e-isolated dry-run golden: OK"
