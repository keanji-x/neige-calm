#!/usr/bin/env bash
# In-container entry for the docker-isolated codex-e2e tier, run by scripts/e2e-isolated/run.sh inside the `--network none` container.
# Config + functions live at top level; main() runs only when executed, so a regression test can source this file and stub proxy_connect.
set -euo pipefail

PROXY_PORT=2081
SOCK=/sock/proxy.sock
# Positive canary: an allowlisted host whose CONNECT must tunnel (200) through
# the full chain. Overridable only for the sourced-function regression path.
FENCE_CANARY="${FENCE_CANARY:-chatgpt.com:443}"
# Targets the gate must refuse with an explicit 403 before dialing upstream; the last is a parsing-trick authority whose raw suffix matches the allowlist but whose host carries injection chars.
FENCE_DENY_TARGETS=(
    127.0.0.1:4040
    127.0.0.1:4041
    10.0.0.1:443
    169.254.169.254:443
    '127.0.0.1:4040#.chatgpt.com:443'
)
# Field distinctive of calm-server's GET /api/version body. NOT a pass/fail
# criterion (the gate's 403 is): kept only to make a breach message conclusive.
PROD_MARKER=kernelVersion

log()  { printf '[e2e-entry] %s\n' "$*" >&2; }
die()  { log "FATAL: $*"; exit 70; }
# Fence exits carry their own code (72 chain-not-live, 71 breach).
fail() { local code="$1"; shift; log "FATAL: $*"; exit "$code"; }

# Only a well-formed FIRST status line yields a code; anything else echoes "" so garbage can never be mistaken for the gate's 403.
parse_http_status() {
    local first="${1%%$'\n'*}"
    first="${first%$'\r'}"
    if [[ "$first" =~ ^HTTP/1\.[01]\ ([0-9][0-9][0-9])($|\ ) ]]; then
        printf '%s' "${BASH_REMATCH[1]}"
    fi
}

proxy_connect() {
    local hostport="$1"
    RESP="$(printf 'CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n' "$hostport" "$hostport" \
        | timeout 25 socat -t 3 -T 3 - "TCP:127.0.0.1:${PROXY_PORT}" 2>/dev/null)" || true
    STATUS="$(parse_http_status "$RESP")"
    [ "${STATUS:-}" = 200 ]
}

fence_assert_allowed() { proxy_connect "$1"; }

# Fail closed: only the gate's 403 passes — a timeout, empty answer, 400/502, dead proxy, or an established 200 must never read as "refused".
fence_assert_denied() {
    proxy_connect "$1" || true
    [ "${STATUS:-}" = 403 ]
}

fence_preflight() {
    local attempt ok=0
    for attempt in 1 2 3; do
        if fence_assert_allowed "$FENCE_CANARY"; then
            ok=1
            log "fence canary OK: CONNECT $FENCE_CANARY -> 200 through the chain (chain live, allowlist admits)"
            break
        fi
        log "fence canary attempt $attempt/3: CONNECT $FENCE_CANARY -> '${STATUS:-no answer}' — retrying"
        sleep 2
    done
    [ "$ok" = 1 ] || fail 72 "positive canary never succeeded — chain not live or allowlist broke; CANNOT PROVE FENCE, aborting before any codex runs"

    local t
    for t in "${FENCE_DENY_TARGETS[@]}"; do
        if fence_assert_denied "$t"; then
            log "fence: CONNECT $t -> 403 refused by our gate (deterministic deny) — OK"
            continue
        fi
        if [ "${STATUS:-}" = 200 ]; then
            log "FENCE BREACH: CONNECT $t was ESTABLISHED (status 200) through the chain — the gate admitted a path to prod."
            if printf '%s' "$RESP" | grep -qi "$PROD_MARKER"; then
                log "FENCE BREACH: response carries '$PROD_MARKER' — PROVEN prod response."
            fi
            printf '%s\n' "$RESP" | head -n 8 >&2
            fail 71 "FENCE BREACH: $t reachable through the forwarder — ABORTING before any codex runs"
        fi
        log "FENCE INDETERMINATE: CONNECT $t -> '${STATUS:-no answer}' (expected our gate's 403). The chain/gate is broken or unreachable — the fence cannot prove denial."
        printf '%s\n' "$RESP" | head -n 8 >&2
        fail 71 "FENCE INDETERMINATE: $t did not return our gate's 403 (got '${STATUS:-no answer}') — fence broken, ABORTING before any codex runs"
    done
    log "fence preflight OK: chain live; prod :4040/:4041 (+ RFC1918/link-local + parse-trick) refused by our gate with an explicit 403 (deny by construction)"
}

SOCAT_PID=""
start_egress_chain() {
    socat "TCP-LISTEN:${PROXY_PORT},bind=127.0.0.1,fork,reuseaddr" "UNIX-CONNECT:${SOCK}" &
    SOCAT_PID=$!
    trap 'kill "$SOCAT_PID" 2>/dev/null || true' EXIT
    local _ ready=0
    for _ in $(seq 1 50); do
        if (exec 3<>"/dev/tcp/127.0.0.1/${PROXY_PORT}") 2>/dev/null; then
            ready=1
            break
        fi
        sleep 0.2
    done
    [ "$ready" = 1 ] || die "in-container socat proxy stub never came up on :${PROXY_PORT}"
    log "proxy chain up: 127.0.0.1:${PROXY_PORT} -> ${SOCK} (terminated host-side by e2e-egress-proxy)"
}

# `gh` is excluded on purpose: the test fixture shims it onto the agent PATH at runtime, so it would false-fail this container-PATH check.
REQUIRED_PATH_TOOLS=(neige rg git zstd bwrap)
assert_required_tools_on_path() {
    local tool missing=()
    for tool in "${REQUIRED_PATH_TOOLS[@]}"; do
        command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
    done
    if [ "${#missing[@]}" -ne 0 ]; then
        log "required CLI(s) missing from PATH: ${missing[*]}"
        log "  each is shelled out as a bare command by the contained agent stack and"
        log "  must be provisioned into the run container — a run.sh bind-mount"
        log "  (workspace binary, e.g. neige -> /usr/local/bin/neige) or a"
        log "  docker/Dockerfile.e2e apt package (rg/git/zstd/bwrap)."
        fail 73 "required CLI(s) not resolvable on PATH: ${missing[*]} — a real codex run would hit 'command not found' mid-suite; ABORTING before any codex runs"
    fi
    log "tool preflight OK: ${REQUIRED_PATH_TOOLS[*]} all resolve on PATH"
}

run_suite() {
    local test_bin="$1" test_filter="$2" decoys="$3"

    declare -A DECOY_PIDS=()
    if [ "$decoys" = 1 ]; then
        mkdir -p /tmp/decoys
        local name
        for name in neige-app calm-server neige-session-daemon; do
            cp /bin/sleep "/tmp/decoys/$name"
            "/tmp/decoys/$name" 100000 &
            DECOY_PIDS[$name]=$!
            log "decoy planted: $name (pid ${DECOY_PIDS[$name]})"
        done
    fi

    kill -0 "$SOCAT_PID" 2>/dev/null || die "in-container socat egress stub died between fence preflight and suite start"
    local args=(--test-threads=1 --nocapture)
    if [ -n "$test_filter" ]; then
        args=("$test_filter" --exact "${args[@]}")
    fi
    log "running: $test_bin ${args[*]}"
    local rc
    set +e
    "$test_bin" "${args[@]}"
    rc=$?
    set -e
    log "suite exit: $rc"

    if [ "$decoys" = 1 ]; then
        local dead=0 name
        for name in neige-app calm-server neige-session-daemon; do
            if kill -0 "${DECOY_PIDS[$name]}" 2>/dev/null; then
                log "decoy survived: $name"
            else
                log "DECOY KILLED: $name — the agent stack still emits name-based kills (contained by the PID namespace, but FIX IT)"
                dead=1
            fi
        done
        log "pgrep -c sleep-decoys: $(pgrep -c -f /tmp/decoys 2>/dev/null || echo 0) still running"
        if [ "$dead" = 1 ] && [ "$rc" -eq 0 ]; then
            rc=97
        fi
    fi

    return "$rc"
}

main() {
    E2E_MODE="${E2E_MODE:-run}"
    E2E_TEST_BIN="${E2E_TEST_BIN:?E2E_TEST_BIN must be set by run.sh}"
    E2E_TEST_FILTER="${E2E_TEST_FILTER:-}"
    DECOYS="${DECOYS:-0}"

    [ -S "$SOCK" ] || die "forwarder unix socket missing at $SOCK"
    [ -r "$HOME/.codex/auth.json" ] || die "auth.json not mounted at \$HOME/.codex/auth.json"
    [ -x /opt/codex/codex ] || die "codex binary not mounted at /opt/codex/codex"
    [ -x "$E2E_TEST_BIN" ] || die "test binary not visible at $E2E_TEST_BIN"

    assert_required_tools_on_path

    start_egress_chain
    fence_preflight

    if [ "$E2E_MODE" = preflight ]; then
        local count
        count="$("$E2E_TEST_BIN" --list 2>&1 | tail -n1)" || die "test binary --list failed (glibc/layout drift?)"
        log "exec probe OK: $count"
        exit 0
    fi

    run_suite "$E2E_TEST_BIN" "$E2E_TEST_FILTER" "$DECOYS"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
fi
