#!/usr/bin/env bash
# #863 — dry-run golden assertions for the docker-isolated codex-e2e tier.
# Runs run.sh --dry-run with a fully fake environment (needs NO docker daemon,
# NO cargo, NO codex install) and asserts the security-critical shape of the
# produced `docker run` argv. Wired as `make e2e-codex-isolated-check`.
#
# NOTE(future hardening idea, not built): put a fake `docker`/`cargo` shim
# first on PATH that records+fails on any invocation — that would prove
# "dry-run executes nothing" positively. Today it is proven by proxy: run.sh
# exits 0 below with fake paths that would make any real docker/cargo call
# fail loudly.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

set +e
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
RC=$?
set -e
if [ "$RC" -ne 0 ]; then
    echo "FAIL: run.sh --dry-run exited rc=$RC; full output:"
    printf '%s\n' "$OUT"
    exit 1
fi

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
must_contain "--user $(id -u):$(id -g)"              # exact non-root uid:gid
must_contain 'seccomp=unconfined'
must_contain 'apparmor=unconfined'
must_contain '--init'
must_contain '--rm'
must_contain '/fake/target:/fake/target:ro'          # target dir ro mount
must_contain '/fake/codex/bin/codex:/opt/codex/codex:ro'
must_contain '/.codex/auth.json:ro'                  # single-file auth mount
must_contain ':/sock:ro'                             # sock dir ro in the run container
must_contain 'NEIGE_CODEX_PROXY=http://127.0.0.1:2081'
must_contain 'NEIGE_CODEX_BIN=/opt/codex/codex'
must_contain '--tmpfs'
must_contain '\,exec\,'                              # tmpfs HOME must be exec (docker default is noexec); %q-escaped comma form
must_contain 'scripts/e2e-isolated/entry.sh'

# repo mounted ro at its identical host path
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
must_contain "$REPO_ROOT:$REPO_ROOT:ro"

# -- exact-token assertions: parse the argv line back into an array --------
# The line is run.sh's own `printf ' %q'` output (a controlled helper we
# fully own), so eval-ing it is the faithful inverse of %q: it reconstructs
# the exact argv tokens with all escaping undone. The raw must_contain /
# must_not_contain substring checks above stay as belt-and-suspenders.
ARGV_LINE="$(printf '%s\n' "$ARGV" | grep '^docker run ' || true)"
if [ -z "$ARGV_LINE" ]; then
    echo "FAIL: no 'docker run ...' line inside the delimited argv block"
    fail=1
else
    declare -a argv=()
    # shellcheck disable=SC2086  # eval of our own %q-printed line IS the parse
    eval "argv=( ${ARGV_LINE#docker run } )"
    n=${#argv[@]}
    if [ "$n" -lt 2 ]; then
        echo "FAIL: parsed argv has only $n tokens"
        fail=1
    else
        user_val="<missing>"
        tmpfs_val="<missing>"
        for ((i = 0; i < n - 1; i++)); do
            case "${argv[i]}" in
                --user)  user_val="${argv[i + 1]}" ;;
                --tmpfs) tmpfs_val="${argv[i + 1]}" ;;
            esac
        done
        # --user must be followed by exactly UID:GID (non-root, no extras)
        want_user="$(id -u):$(id -g)"
        if [ "$user_val" != "$want_user" ]; then
            echo "FAIL: --user value is '$user_val' (want exactly '$want_user')"
            fail=1
        fi
        # tmpfs HOME must carry exec after unescaping (docker default: noexec)
        case "$tmpfs_val" in
            *,exec,*) : ;;
            *) echo "FAIL: --tmpfs value '$tmpfs_val' lacks ',exec,' after unescaping"; fail=1 ;;
        esac
        # the argv must END at the entry script — nothing may ride after it
        if [ "${argv[n - 1]}" != "$REPO_ROOT/scripts/e2e-isolated/entry.sh" ]; then
            echo "FAIL: argv does not end at $REPO_ROOT/scripts/e2e-isolated/entry.sh (last token: '${argv[n - 1]}')"
            fail=1
        fi
    fi
fi

# -- forbidden: anything that would widen the blast radius --
must_not_contain '--publish'
must_not_contain ' -p '
must_not_contain ' -P '
must_not_contain '--cap-add'
must_not_contain 'docker.sock'
must_not_contain '--pid '                            # (substring-safe: does not match --pids-limit)
must_not_contain '--pid='
must_not_contain '--privileged'
must_not_contain '--network host'
must_not_contain '--network bridge'
must_not_contain '--network='
must_not_contain ' --net '
must_not_contain '--net='
must_not_contain '--userns'
must_not_contain '--ipc'
must_not_contain '--uts'

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
