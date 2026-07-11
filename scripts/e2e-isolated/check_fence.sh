#!/usr/bin/env bash
# #923 F1 — regression for entry.sh's fence VERDICT + preflight EXIT contract.
#
# The fence must FAIL CLOSED: a denied target passes ONLY on our gate's
# deterministic 403. A timeout / empty / 400 / 502 / dead-proxy answer (or an
# established 200) must NOT read as "denied" — the old bug counted any non-200
# as "refused", so a DOWN proxy passed the fence (fail OPEN).
#
# Pure shell, NO docker: source entry.sh (its BASH_SOURCE guard suppresses
# main), stub proxy_connect to yield a chosen STATUS, and assert the verdict
# functions + the fence_preflight exit codes.
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=entry.sh
source "$SCRIPT_DIR/entry.sh"

# Sourcing entry.sh turns on `set -e`; the assertions below intentionally drive
# functions that return non-zero, so disable it for the test body. (entry.sh's
# own log/die/fail helpers are LEFT intact — fence_preflight relies on `fail`
# actually calling exit, which the preflight exit-code cases below assert.)
set +e

FAILS=0
ok()  { printf '[check_fence] ok   : %s\n' "$1"; }
bad() { printf '[check_fence] FAIL : %s\n' "$1" >&2; FAILS=$((FAILS + 1)); }

# Stub proxy_connect: set STATUS/RESP from the per-target knobs and mimic the
# real return (0 iff 200). The canary target reads $STUB_CANARY; every other
# target reads $STUB_DENY.
STUB_CANARY=""
STUB_DENY=""
proxy_connect() {
    case "$1" in
        "$FENCE_CANARY") STATUS="$STUB_CANARY" ;;
        *)               STATUS="$STUB_DENY" ;;
    esac
    RESP="HTTP/1.1 ${STATUS:-000} stub"
    [ "${STATUS:-}" = 200 ]
}
# Make the canary retry-sleep instant so the dead-chain case does not stall.
sleep() { :; }

# ---- verdict: fence_assert_denied passes ONLY on an explicit 403 ----------
for s in "" 400 502 200; do
    STUB_DENY="$s"
    if fence_assert_denied "denied.example:443"; then
        bad "fence_assert_denied must REJECT status '${s:-empty}' (only 403 passes)"
    else
        ok "fence_assert_denied rejects status '${s:-empty}'"
    fi
done
STUB_DENY=403
if fence_assert_denied "denied.example:443"; then
    ok "fence_assert_denied accepts an explicit 403"
else
    bad "fence_assert_denied must accept an explicit 403"
fi

# ---- verdict: canary passes ONLY on 200 -----------------------------------
for s in "" 400 403 502; do
    STUB_CANARY="$s"
    if fence_assert_allowed "$FENCE_CANARY"; then
        bad "canary must REJECT status '${s:-empty}' (only 200 passes)"
    else
        ok "canary rejects status '${s:-empty}'"
    fi
done
STUB_CANARY=200
if fence_assert_allowed "$FENCE_CANARY"; then
    ok "canary accepts 200"
else
    bad "canary must accept 200"
fi

# ---- preflight EXIT contract (subshell captures the exit code) ------------
# fence_preflight calls entry.sh's real `fail`, which exits; run it in a
# subshell so we can read the code instead of exiting this harness.
run_preflight() { # $1 canary-status $2 deny-status -> echoes the exit code
    STUB_CANARY="$1"
    STUB_DENY="$2"
    ( fence_preflight ) >/dev/null 2>&1
    printf '%s' "$?"
}
assert_exit() { # $1 label $2 want $3 got
    if [ "$2" = "$3" ]; then ok "$1 (exit $3)"; else bad "$1: want exit $2, got $3"; fi
}
assert_exit "preflight all-good (canary 200, deny 403) succeeds" 0  "$(run_preflight 200 403)"
assert_exit "preflight breach (deny 200) aborts 71"             71 "$(run_preflight 200 200)"
assert_exit "preflight indeterminate (deny 502) aborts 71"      71 "$(run_preflight 200 502)"
assert_exit "preflight indeterminate (deny empty) aborts 71"    71 "$(run_preflight 200 '')"
assert_exit "preflight dead chain (canary never 200) aborts 72" 72 "$(run_preflight '' 403)"

if [ "$FAILS" -eq 0 ]; then
    printf '[check_fence] ALL PASS\n'
    exit 0
fi
printf '[check_fence] %d FAILURE(S)\n' "$FAILS" >&2
exit 1
