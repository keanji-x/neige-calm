#!/usr/bin/env bash
# In-container entry for the #863 docker-isolated codex-e2e tier.
# Invoked by scripts/e2e-isolated/run.sh inside the `--network none` run
# container (the repo is mounted read-only at its host path, so this file is
# executed straight off that mount). Responsibilities, in order:
#   1. Bring up the only egress: socat loopback TCP :2081 → /sock/proxy.sock
#      (→ host forwarder → host proxy). NEIGE_CODEX_PROXY points here.
#   2. REQUIRED fence preflight, three steps, all before any codex process:
#      (a) REMOTE POSITIVE CANARY — a GET to a public URL through the SAME
#          chain MUST succeed. Without it a dead chain would make prod
#          "unreachable" vacuously and the fence would pass fail-open;
#          the canary makes the fence provable. Chain dead → ABORT (72).
#      (b) FINGERPRINT CALIBRATION — probe a guaranteed-dead destination
#          (127.0.0.1:1, the fixture's dead-ingest port, never listening)
#          through the same chain TWICE. Whatever comes back (or doesn't)
#          is by definition the proxy's own unreachable-destination
#          behavior; the two probes are diffed so only empirically-STABLE
#          dimensions (status line, header shape, body) are enforced.
#          Unstable status/headers → cannot calibrate = cannot prove the
#          fence → ABORT (73).
#      (c) prod 127.0.0.1:4040/:4041 probed through the chain. FAIL-CLOSED
#          ALLOWLIST: no HTTP answer at all = unreachable = OK; an answer
#          matching the dead-destination fingerprint in every stable
#          dimension = the proxy's own error = OK; ANY other answer,
#          regardless of status class, = something reachable answered →
#          ABORT (71). (No marker heuristic: prod's /api/version marker is
#          kept only as an extra loud confirmation on breach.)
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
# Guaranteed-dead destination for fingerprint calibration: the fixture's
# dead-ingest port — nothing ever listens on it, on this host or the remote.
DEAD_TARGET=127.0.0.1:1
# Same path for calibration and prod probes: if the proxy error echoes the
# URL, the echoes differ only by target host:port, which resp_body_norm()
# masks before comparison.
FENCE_PATH=/api/version
# Field name distinctive of calm-server's GET /api/version JSON body. NOT a
# pass/fail criterion anymore (the fingerprint allowlist subsumes it); kept
# only to make a breach message conclusive when it matches.
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

# (b) calibrate the proxy-error FINGERPRINT off a guaranteed-dead destination.
# Rationale (fail-closed allowlist): instead of classifying prod answers by
# what they LACK (the old marker-absence blocklist — spoofable by any error
# page that happens not to say kernelVersion), we learn what the proxy's own
# unreachable-destination behavior IS, and later accept ONLY that. Anything
# else answering on a prod port is, by elimination, not the proxy erroring —
# it is something reachable.
resp_status_line() { printf '%s' "$RESP" | head -n1 | tr -d '\r'; }
resp_header_shape() {
    # "Shape" = sorted lowercase NAMES of all headers (values like Date vary
    # legitimately between two probes) + the full lowercased Server / Via /
    # Proxy-* lines (they identify the answering software and must not move).
    local hdrs
    hdrs="$(printf '%s' "$RESP" | tr -d '\r' | sed -n '2,/^$/p' | sed '/^$/d' | tr '[:upper:]' '[:lower:]')"
    printf '%s\n' "$hdrs" | sed -n 's/^\([^:[:space:]]*\):.*$/name:\1/p' | sort
    printf '%s\n' "$hdrs" | grep -E '^(server|via|proxy-)[^:]*:' | sort || true
}
resp_body_norm() {
    # Body with the probed target masked: a proxy error page legitimately
    # echoes the destination it failed to reach; every other byte must match.
    local target="$1" body
    body="$(printf '%s' "$RESP" | sed -e '1,/^\r\{0,1\}$/d')"
    printf '%s' "${body//"$target"/<TARGET>}"
}
body_sha() { printf '%s' "$1" | sha256sum | awk '{print $1}'; }

FP_PRESENT="" FP_STATUS_LINE="" FP_HEADERS="" FP_BODY_MODE="" FP_BODY_HASH="" FP_BODY_SIZE=""
fp_calibrate() {
    local i
    local -a c_present=() c_status=() c_hdrs=() c_body=()
    for i in 1 2; do
        proxy_get "$DEAD_TARGET" "$FENCE_PATH"
        if [ -z "$STATUS" ]; then
            c_present[i]=0 c_status[i]="" c_hdrs[i]="" c_body[i]=""
            log "fingerprint probe $i/2: http://$DEAD_TARGET$FENCE_PATH -> no HTTP answer"
        else
            c_present[i]=1
            c_status[i]="$(resp_status_line)"
            c_hdrs[i]="$(resp_header_shape)"
            c_body[i]="$(resp_body_norm "$DEAD_TARGET")"
            log "fingerprint probe $i/2: http://$DEAD_TARGET$FENCE_PATH -> '${c_status[i]}' ($(printf '%s' "${c_body[i]}" | wc -c) body bytes)"
        fi
    done
    # Diff the two probes: only dimensions stable across both are enforceable.
    # Unstable presence/status/headers → we cannot say what "the proxy's own
    # error" looks like → cannot prove the fence → ABORT (73).
    if [ "${c_present[1]}" != "${c_present[2]}" ]; then
        log "FATAL: dead-destination probes disagree on whether an HTTP answer comes back at all — unstable fingerprint, cannot calibrate = cannot prove fence."
        exit 73
    fi
    FP_PRESENT="${c_present[1]}"
    if [ "$FP_PRESENT" = 0 ]; then
        log "fingerprint calibrated: dead destinations yield NO HTTP answer through this chain — ANY HTTP answer from a prod probe is a breach."
        return 0
    fi
    if [ "${c_status[1]}" != "${c_status[2]}" ]; then
        log "FATAL: dead-destination status lines disagree ('${c_status[1]}' vs '${c_status[2]}') — unstable fingerprint, cannot calibrate = cannot prove fence."
        exit 73
    fi
    if [ "${c_hdrs[1]}" != "${c_hdrs[2]}" ]; then
        log "FATAL: dead-destination header shapes disagree — unstable fingerprint, cannot calibrate = cannot prove fence."
        log "shape 1:"; printf '%s\n' "${c_hdrs[1]}" | sed 's/^/[e2e-entry]   /' >&2
        log "shape 2:"; printf '%s\n' "${c_hdrs[2]}" | sed 's/^/[e2e-entry]   /' >&2
        exit 73
    fi
    FP_STATUS_LINE="${c_status[1]}"
    FP_HEADERS="${c_hdrs[1]}"
    FP_BODY_SIZE="$(printf '%s' "${c_body[1]}" | wc -c)"
    if [ "${c_body[1]}" = "${c_body[2]}" ]; then
        FP_BODY_MODE="hash"
        FP_BODY_HASH="$(body_sha "${c_body[1]}")"
    elif [ "$FP_BODY_SIZE" = "$(printf '%s' "${c_body[2]}" | wc -c)" ]; then
        FP_BODY_MODE="size"
        log "NOTICE: dead-destination bodies differ byte-wise but agree on size — only size ($FP_BODY_SIZE) joins the fingerprint (unstable bytes excluded)."
    else
        FP_BODY_MODE="none"
        log "NOTICE: dead-destination bodies are unstable ($FP_BODY_SIZE vs $(printf '%s' "${c_body[2]}" | wc -c) bytes) — body excluded from the fingerprint; status line + header shape still enforced."
    fi
    log "fingerprint calibrated: status='$FP_STATUS_LINE' body-mode=$FP_BODY_MODE${FP_BODY_HASH:+ sha256=$FP_BODY_HASH}${FP_BODY_SIZE:+ size=$FP_BODY_SIZE}"
    log "fingerprint header shape:"
    printf '%s\n' "$FP_HEADERS" | sed 's/^/[e2e-entry]   /' >&2
    log "fingerprint body head: $(printf '%s' "${c_body[1]}" | head -c 160 | tr -d '\r\n')"
}
fp_calibrate

# (c) prod must be unreachable through the (proven-live, now calibrated)
# chain. ALLOWLIST: OK is only "no HTTP answer" or "answer == fingerprint in
# every stable dimension". Any other answer aborts, whatever its status.
fence_probe() {
    local target="$1"
    proxy_get "$target" "$FENCE_PATH"
    if [ -z "$STATUS" ]; then
        log "fence: $target -> no HTTP answer (unreachable through the chain) — OK"
        return 0
    fi
    local s h b bs why=""
    s="$(resp_status_line)"
    h="$(resp_header_shape)"
    b="$(resp_body_norm "$target")"
    bs="$(printf '%s' "$b" | wc -c)"
    if [ "$FP_PRESENT" = 0 ]; then
        why="dead destinations yield NO HTTP answer through this chain, yet $target answered"
    elif [ "$s" != "$FP_STATUS_LINE" ]; then
        why="status line '$s' != fingerprint '$FP_STATUS_LINE'"
    elif [ "$h" != "$FP_HEADERS" ]; then
        why="header shape differs from fingerprint"
    elif [ "$FP_BODY_MODE" = hash ] && [ "$(body_sha "$b")" != "$FP_BODY_HASH" ]; then
        why="body sha256 differs from fingerprint (sizes: $bs vs $FP_BODY_SIZE)"
    elif [ "$FP_BODY_MODE" = size ] && [ "$bs" != "$FP_BODY_SIZE" ]; then
        why="body size $bs != fingerprint size $FP_BODY_SIZE"
    fi
    if [ -z "$why" ]; then
        log "fence: $target -> '$s' matches the dead-destination proxy-error fingerprint — OK"
        return 0
    fi
    log "FENCE BREACH: $target answered through the proxy chain, and the answer is NOT the proxy's own dead-destination error: $why."
    if printf '%s' "$RESP" | grep -qi "$PROD_MARKER"; then
        log "FENCE BREACH: response carries '$PROD_MARKER' — PROVEN prod response."
    fi
    if [ "$h" != "$FP_HEADERS" ]; then
        log "observed header shape:"; printf '%s\n' "$h" | sed 's/^/[e2e-entry]   /' >&2
        log "fingerprint header shape:"; printf '%s\n' "$FP_HEADERS" | sed 's/^/[e2e-entry]   /' >&2
    fi
    printf '%s\n' "$RESP" | head -n 12 >&2
    log "sing-box routing would hand agents a path to prod — ABORTING before any codex runs."
    return 1
}
fence_probe 127.0.0.1:4040 || exit 71
fence_probe 127.0.0.1:4041 || exit 71
log "fence preflight OK: chain live, prod :4040/:4041 answer only as the proxy's own dead-destination error (or not at all)"

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
