#!/usr/bin/env bash
# In-container entry for the #863 docker-isolated codex-e2e tier.
# Invoked by scripts/e2e-isolated/run.sh inside the `--network none` run
# container (the repo is mounted read-only at its host path, so this file is
# executed straight off that mount). Responsibilities, in order:
#   1. Bring up the only egress: socat loopback TCP :2081 → /sock/proxy.sock
#      (→ host forwarder → host proxy). NEIGE_CODEX_PROXY points here.
#   2. REQUIRED fence preflight: prod 127.0.0.1:4040/:4041 must NOT be
#      reachable THROUGH that chain. Fail-closed: any origin-looking HTTP
#      answer aborts; a 200 carrying a version string is the proven-prod
#      signature. Runs before any codex process exists.
#   3. E2E_MODE=preflight → `--list` exec probe (glibc/mount-layout proof),
#      then exit. E2E_MODE=run → optional DECOYS, then the suite.
#   4. DECOYS=1: plant /bin/sleep copies named like prod processes; if any
#      dies during the run, the agent stack still emits name-based kills —
#      loud regression telemetry (exit 97), even though the PID namespace
#      already contains it.
set -euo pipefail

E2E_MODE="${E2E_MODE:-run}"
E2E_TEST_BIN="${E2E_TEST_BIN:?E2E_TEST_BIN must be set by run.sh}"
E2E_TEST_FILTER="${E2E_TEST_FILTER:-}"
DECOYS="${DECOYS:-0}"
PROXY_PORT=2081
SOCK=/sock/proxy.sock

log() { printf '[e2e-entry] %s\n' "$*" >&2; }
die() { log "FATAL: $*"; exit 70; }

# ---- mount-layout assertions (catches bind/tmpfs ordering regressions) ----
[ -S "$SOCK" ] || die "forwarder unix socket missing at $SOCK"
[ -r "$HOME/.codex/auth.json" ] || die "auth.json not mounted at \$HOME/.codex/auth.json"
[ -x /opt/codex/codex ] || die "codex binary not mounted at /opt/codex/codex"
[ -x "$E2E_TEST_BIN" ] || die "test binary not visible at $E2E_TEST_BIN"

# ---- 1. proxy chain --------------------------------------------------------
socat "TCP-LISTEN:${PROXY_PORT},bind=127.0.0.1,fork,reuseaddr" "UNIX-CONNECT:${SOCK}" &
SOCAT_PID=$!
ready=0
for _ in $(seq 1 50); do
    if (exec 3<>"/dev/tcp/127.0.0.1/${PROXY_PORT}") 2>/dev/null; then
        ready=1
        break
    fi
    sleep 0.2
done
[ "$ready" = 1 ] || die "in-container socat proxy stub never came up on :${PROXY_PORT}"
log "proxy chain up: 127.0.0.1:${PROXY_PORT} -> ${SOCK}"

# ---- 2. fence preflight (design §B — asserted EVERY run, never assumed) ----
# HTTP client via socat (in-image; no curl): absolute-form GET through the
# proxy, HTTP/1.0 + Connection: close so the exchange self-terminates.
fence_probe() {
    local target="$1" resp status
    resp="$(printf 'GET http://%s/api/version HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n' "$target" "$target" \
        | timeout 20 socat -t 15 -T 15 - "TCP:127.0.0.1:${PROXY_PORT}" 2>/dev/null)" || true
    status="$(printf '%s' "$resp" | head -n1 | awk '/^HTTP\//{print $2}')"
    if [ -z "$status" ]; then
        log "fence: $target -> no HTTP answer (unreachable) — OK"
        return 0
    fi
    case "$status" in
        5??)
            log "fence: $target -> proxy/gateway error $status (tunneled elsewhere / refused) — OK"
            return 0
            ;;
        *)
            log "FENCE BREACH: $target answered HTTP $status through the proxy chain."
            if printf '%s' "$resp" | grep -qi 'version'; then
                log "FENCE BREACH: response carries a version string — this IS prod."
            fi
            printf '%s\n' "$resp" | head -n 12 >&2
            log "sing-box routing would hand agents a path to prod — ABORTING before any codex runs."
            return 1
            ;;
    esac
}
fence_probe 127.0.0.1:4040 || exit 71
fence_probe 127.0.0.1:4041 || exit 71
log "fence preflight OK: prod :4040/:4041 unreachable through the chain"

# ---- 3. preflight mode: exec probe only ------------------------------------
if [ "$E2E_MODE" = preflight ]; then
    count="$("$E2E_TEST_BIN" --list 2>&1 | tail -n1)" || die "test binary --list failed (glibc/layout drift?)"
    log "exec probe OK: $count"
    kill "$SOCAT_PID" 2>/dev/null || true
    exit 0
fi

# ---- 4. decoys (opt-in) -----------------------------------------------------
declare -A DECOY_PIDS=()
if [ "$DECOYS" = 1 ]; then
    mkdir -p /tmp/decoys
    for name in neige-app calm-server neige-session-daemon; do
        cp /bin/sleep "/tmp/decoys/$name"
        "/tmp/decoys/$name" 100000 &
        DECOY_PIDS[$name]=$!
        log "decoy planted: $name (pid ${DECOY_PIDS[$name]})"
    done
fi

# ---- 5. the suite -----------------------------------------------------------
args=(--test-threads=1 --nocapture)
if [ -n "$E2E_TEST_FILTER" ]; then
    args=("$E2E_TEST_FILTER" --exact "${args[@]}")
fi
log "running: $E2E_TEST_BIN ${args[*]}"
set +e
"$E2E_TEST_BIN" "${args[@]}"
rc=$?
set -e
log "suite exit: $rc"

if [ "$DECOYS" = 1 ]; then
    dead=0
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

kill "$SOCAT_PID" 2>/dev/null || true
exit "$rc"
