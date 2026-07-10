#!/usr/bin/env bash
# In-container entry for the #863 docker-isolated codex-e2e tier.
# Invoked by scripts/e2e-isolated/run.sh inside the `--network none` run
# container (the repo is mounted read-only at its host path, so this file is
# executed straight off that mount). Responsibilities, in order:
#   1. Bring up the only egress: socat loopback TCP :2081 → /sock/proxy.sock
#      (→ host forwarder → host proxy). NEIGE_CODEX_PROXY points here.
#   2. REQUIRED fence preflight, two halves, both before any codex process:
#      (a) REMOTE POSITIVE CANARY — a GET to a public URL through the SAME
#          chain MUST succeed. Without it a dead chain would make prod
#          "unreachable" vacuously and the fence would pass fail-open;
#          the canary makes the fence provable. Chain dead → ABORT (72).
#      (b) prod 127.0.0.1:4040/:4041 must NOT be reachable THROUGH the
#          chain. Fail-closed: any non-5xx HTTP answer aborts, and a 5xx
#          whose body carries prod's /api/version marker (a reachable prod
#          erroring 500) aborts too; only a genuine proxy/gateway 5xx
#          without the marker — or no HTTP answer at all — passes (71).
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
# A public URL the chain must be able to fetch (plain-http absolute-form GET
# — what socat speaks cleanly; TLS/CONNECT is exercised later by codex
# itself). Any 2xx/3xx proves the socat→forwarder→proxy chain is live.
CANARY_HOST=example.com
# Field name distinctive of calm-server's GET /api/version JSON body. A 5xx
# carrying it is a REACHABLE prod erroring — not a proxy gateway error.
PROD_MARKER=kernelVersion

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
# Explicit lifecycle for the backgrounded stub: tini (--init) would collapse
# the namespace on exit anyway, but be explicit so no path leaves it behind.
trap 'kill "$SOCAT_PID" 2>/dev/null || true' EXIT
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
proxy_get() {
    # GET http://$1$2 through the in-container stub.
    # Sets RESP (raw response) and STATUS ("" when no HTTP answer came back).
    local host="$1" path="$2"
    RESP="$(printf 'GET http://%s%s HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n' "$host" "$path" "$host" \
        | timeout 20 socat -t 15 -T 15 - "TCP:127.0.0.1:${PROXY_PORT}" 2>/dev/null)" || true
    STATUS="$(printf '%s' "$RESP" | head -n1 | awk '/^HTTP\//{print $2}')"
}

# (a) remote positive canary — REQUIRED to succeed, else the fence below
# proves nothing (a broken chain must not pass as "prod unreachable").
canary_ok=0
for attempt in 1 2 3; do
    proxy_get "$CANARY_HOST" /
    case "$STATUS" in
        2??|3??)
            canary_ok=1
            log "fence canary OK: http://$CANARY_HOST/ -> HTTP $STATUS through the chain (chain is live)"
            break
            ;;
    esac
    log "fence canary attempt $attempt/3: http://$CANARY_HOST/ -> '${STATUS:-no HTTP answer}' — retrying"
    sleep 2
done
if [ "$canary_ok" != 1 ]; then
    log "FATAL: remote canary never succeeded — chain not live, CANNOT PROVE FENCE; aborting before any codex runs."
    exit 72
fi

# (b) prod must be unreachable through the (now proven-live) chain.
fence_probe() {
    local target="$1"
    proxy_get "$target" /api/version
    if [ -z "$STATUS" ]; then
        log "fence: $target -> no HTTP answer (unreachable) — OK"
        return 0
    fi
    case "$STATUS" in
        5??)
            if printf '%s' "$RESP" | grep -qi "$PROD_MARKER"; then
                log "FENCE BREACH: $target -> HTTP $STATUS but the body carries '$PROD_MARKER' — a REACHABLE prod answering with an error status."
                printf '%s\n' "$RESP" | head -n 12 >&2
                log "sing-box routing would hand agents a path to prod — ABORTING before any codex runs."
                return 1
            fi
            log "fence: $target -> proxy/gateway error $STATUS without '$PROD_MARKER' (tunneled elsewhere / refused) — OK"
            return 0
            ;;
        *)
            log "FENCE BREACH: $target answered HTTP $STATUS through the proxy chain."
            if printf '%s' "$RESP" | grep -qi "$PROD_MARKER"; then
                log "FENCE BREACH: response carries '$PROD_MARKER' — this IS prod."
            fi
            printf '%s\n' "$RESP" | head -n 12 >&2
            log "sing-box routing would hand agents a path to prod — ABORTING before any codex runs."
            return 1
            ;;
    esac
}
fence_probe 127.0.0.1:4040 || exit 71
fence_probe 127.0.0.1:4041 || exit 71
log "fence preflight OK: chain live, prod :4040/:4041 unreachable through it"

# ---- 3. preflight mode: exec probe only ------------------------------------
if [ "$E2E_MODE" = preflight ]; then
    count="$("$E2E_TEST_BIN" --list 2>&1 | tail -n1)" || die "test binary --list failed (glibc/layout drift?)"
    log "exec probe OK: $count"
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
# The egress stub must still be alive right before the suite starts — a dead
# socat here would strand every codex API call mid-run.
kill -0 "$SOCAT_PID" 2>/dev/null || die "in-container socat egress stub died between fence preflight and suite start"
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

exit "$rc"
