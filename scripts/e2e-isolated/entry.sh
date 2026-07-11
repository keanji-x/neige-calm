#!/usr/bin/env bash
# In-container entry for the #863 docker-isolated codex-e2e tier.
# Invoked by scripts/e2e-isolated/run.sh inside the `--network none` run
# container (the repo is mounted read-only at its host path, so this file is
# executed straight off that mount). Responsibilities, in order:
#   1. Bring up the only egress: socat loopback TCP :2081 → /sock/proxy.sock.
#      That socket is terminated HOST-SIDE by our deterministic gate
#      `e2e-egress-proxy` (the sole terminator; run.sh forwarder), which forwards
#      only allowlisted CONNECTs upstream to sing-box. NEIGE_CODEX_PROXY points
#      at :2081. The in-container socat is a dumb relay INTO our gate — it makes
#      no policy decision (a rogue codex could connect(2) /sock directly, so the
#      gate, not this relay, is the boundary; design §4).
#   2. REQUIRED fence preflight, a DETERMINISTIC assertion (design §5), before
#      any codex process:
#      (a) POSITIVE CANARY — a CONNECT to an allowlisted host (chatgpt.com:443)
#          through the SAME chain MUST return 200. It proves the chain is live
#          (socat → gate → sing-box) and the allowlist admits; without it a dead
#          chain would make prod "unreachable" vacuously (fail-open). Dead → 72.
#      (b) NEGATIVE ASSERTION — CONNECT 127.0.0.1:4040/:4041 (prod) + 10.0.0.1
#          /169.254.169.254:443 (RFC1918 / link-local) through the chain MUST be
#          REFUSED (403). Our gate decides the denial BEFORE it ever dials
#          sing-box (prod fails the positive host+port allowlist: wrong port
#          and/or wrong host), so the outcome is deterministic regardless of
#          sing-box jitter/routing. Any prod CONNECT that is ESTABLISHED is a
#          breach → 71. No fingerprint, no calibration, no DEAD_TARGETS: deny is
#          by construction, not inference. (Defense in depth: if a refused
#          target ever leaks a body containing the prod marker, shout louder.)
#   3. E2E_MODE=preflight → `--list` exec probe (glibc/mount-layout proof),
#      then exit. E2E_MODE=run → optional DECOYS, then the suite.
#   4. DECOYS=1: plant /bin/sleep copies named like prod processes; if any
#      dies during the run, the agent stack still emits name-based kills —
#      loud regression telemetry (exit 97), even though the PID namespace
#      already contains it.
#
# Structure: config + functions are defined at top level; the procedural flow
# lives in main(), run only when EXECUTED (`bash entry.sh`), not when SOURCED
# (BASH_SOURCE exec guard at the end) — so a regression test can source this
# file, stub proxy_connect, and drive fence_preflight in isolation. (The
# security boundary itself is unit-tested in crates/e2e-egress-proxy.)
set -euo pipefail

PROXY_PORT=2081
SOCK=/sock/proxy.sock
# Positive canary: an allowlisted host whose CONNECT must tunnel (200) through
# the full chain. Overridable only for the sourced-function regression path.
FENCE_CANARY="${FENCE_CANARY:-chatgpt.com:443}"
# Targets our gate MUST refuse with an EXPLICIT 403: prod (wrong port) + RFC1918
# / link-local (wrong host) on :443, plus a parsing-trick authority whose raw
# suffix string-matches the allowlist but whose host carries injection chars
# (`:` `#`). The last one proves the deny is by construction (charset + host +
# port gate), NOT by the upstream parser's leniency. All are denied before the
# gate dials upstream, so refusal is deterministic.
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

# ---- fence primitives (sourceable) -----------------------------------------
# CONNECT $1 (host:port) through the in-container chain (:2081 → /sock → host
# gate → sing-box). Sets RESP (raw response) + STATUS (numeric HTTP code parsed
# from the "HTTP/1.1 NNN" status line, "" on no answer/timeout). STATUS is THE
# verdict source: our gate answers a deterministic 403 for a denied target and
# 200 for an established tunnel. Returns 0 iff STATUS==200 (used by the positive
# canary); the negative path reads STATUS directly so a NON-answer can never be
# mistaken for a refusal. HTTP/1.1 CONNECT is what codex speaks to
# NEIGE_CODEX_PROXY; short idle timeouts because an established tunnel then sits
# silent (we send no TLS) and a 403 closes at once.
# Strict status-line parse: echo the 3-digit code ONLY when the FIRST line is a
# well-formed `HTTP/1.0 NNN ...` or `HTTP/1.1 NNN ...`; anything else echoes ""
# so a malformed/garbage line can NEVER be mistaken for our gate's 403 (the
# deny-by-construction verdict must come from a real status line, not a loose
# field split). Only the first line counts — a later "403" cannot rescue junk.
parse_http_status() {
    # $1 = raw response. Take line 1, strip a trailing CR, regex-match the grammar.
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

# Positive canary: PASS iff our gate ESTABLISHED the tunnel (200). Anything else
# (403 / timeout / empty) is a canary failure.
fence_assert_allowed() { proxy_connect "$1"; }

# Negative assertion: PASS (return 0) ONLY on our gate's deterministic 403.
# FAIL CLOSED — a timeout, empty answer, 400/502, a DOWN proxy, OR an established
# 200 all leave STATUS != 403 and return non-zero, so the caller treats them as
# a fence failure instead of silently reading "no answer" as "refused" (the old
# fail-OPEN bug: any non-200 counted as denied, so even a dead proxy "passed").
fence_assert_denied() {
    proxy_connect "$1" || true
    [ "${STATUS:-}" = 403 ]
}

fence_preflight() {
    # (a) POSITIVE: an allowlisted host must CONNECT (chain live + allowlist OK).
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

    # (b) NEGATIVE (deterministic): the gate must REFUSE prod + private targets
    #     with an EXPLICIT 403. A 403 is the ONLY pass. An established 200 is a
    #     breach; ANYTHING else (timeout/empty/400/502/dead proxy) leaves the
    #     denial UNPROVEN. Both abort 71 — the fence fails closed and never
    #     silently accepts a non-answer as "refused".
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
        # Neither a clean 403 nor an established 200: we could not obtain our
        # gate's deterministic denial, so the fence cannot PROVE prod is
        # unreachable. Indeterminate == broken == abort (fail closed).
        log "FENCE INDETERMINATE: CONNECT $t -> '${STATUS:-no answer}' (expected our gate's 403). The chain/gate is broken or unreachable — the fence cannot prove denial."
        printf '%s\n' "$RESP" | head -n 8 >&2
        fail 71 "FENCE INDETERMINATE: $t did not return our gate's 403 (got '${STATUS:-no answer}') — fence broken, ABORTING before any codex runs"
    done
    log "fence preflight OK: chain live; prod :4040/:4041 (+ RFC1918/link-local + parse-trick) refused by our gate with an explicit 403 (deny by construction)"
}

# ---- egress chain (sourceable) ---------------------------------------------
# Bring up the in-container relay and block until it accepts. Sets SOCAT_PID and
# installs the EXIT trap that reaps it.
SOCAT_PID=""
start_egress_chain() {
    socat "TCP-LISTEN:${PROXY_PORT},bind=127.0.0.1,fork,reuseaddr" "UNIX-CONNECT:${SOCK}" &
    SOCAT_PID=$!
    # Explicit lifecycle for the backgrounded stub: tini (--init) would collapse
    # the namespace on exit anyway, but be explicit so no path leaves it behind.
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

# ---- suite (sourceable) ----------------------------------------------------
run_suite() {
    local test_bin="$1" test_filter="$2" decoys="$3"

    # ---- decoys (opt-in) ----------------------------------------------------
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

    # The egress stub must still be alive right before the suite starts — a dead
    # socat here would strand every codex API call mid-run.
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

# ---- main (executed, not sourced) ------------------------------------------
main() {
    E2E_MODE="${E2E_MODE:-run}"
    E2E_TEST_BIN="${E2E_TEST_BIN:?E2E_TEST_BIN must be set by run.sh}"
    E2E_TEST_FILTER="${E2E_TEST_FILTER:-}"
    DECOYS="${DECOYS:-0}"

    # ---- mount-layout assertions (catches bind/tmpfs ordering regressions) --
    [ -S "$SOCK" ] || die "forwarder unix socket missing at $SOCK"
    [ -r "$HOME/.codex/auth.json" ] || die "auth.json not mounted at \$HOME/.codex/auth.json"
    [ -x /opt/codex/codex ] || die "codex binary not mounted at /opt/codex/codex"
    [ -x "$E2E_TEST_BIN" ] || die "test binary not visible at $E2E_TEST_BIN"

    start_egress_chain
    fence_preflight

    # ---- preflight mode: exec probe only ------------------------------------
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
