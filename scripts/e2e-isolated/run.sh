#!/usr/bin/env bash
# =============================================================================
# Docker-isolated codex-e2e tier runner: host-compiles the calm-server
# `codex_forge_e2e` suite and runs it in a `--network none` container whose only
# egress is the host-side `e2e-egress-proxy` gate (deny by construction).
#
# Usage:
#   scripts/e2e-isolated/run.sh                      # whole suite
#   scripts/e2e-isolated/run.sh --test NAME          # one test (--exact)
#   scripts/e2e-isolated/run.sh --dry-run            # print argv, run nothing
#   scripts/e2e-isolated/run.sh --preflight-only     # build+image+forwarder+
#                                                    # fence + `--list` probe,
#                                                    # no codex, then stop
#   scripts/e2e-isolated/run.sh --forwarder-only     # ensure forwarder, stop
#                                                    # (needs no credentials)
#   scripts/e2e-isolated/run.sh --forwarder-down     # guarded teardown of the
#                                                    # forwarder + socket dir
#   scripts/e2e-isolated/run.sh --no-build           # reuse existing binary
#   scripts/e2e-isolated/run.sh --test-bin PATH      # explicit test binary
#   DECOYS=1 scripts/e2e-isolated/run.sh             # plant name-decoy
#                                                    # processes, assert they
#                                                    # survive (regression
#                                                    # telemetry)
#
# Opt-in budget overrides (env, forwarded only when set in the runner's env):
#   NEIGE_PLANNER_PLANNING_BUDGET=<sec>    planner plan.updated wait         (def 240)
#   NEIGE_CODEX_FORGE_E2E_BUDGET=<sec>  worker/worktree.committed wait (def 180)
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"

CALM_HOST_PROXY_HOST="${CALM_HOST_PROXY_HOST:-127.0.0.1}"
# Deliberately no default port: an empty value must fail loudly below (the Makefile injects it from the host .env).
CALM_HOST_PROXY_PORT="${CALM_HOST_PROXY_PORT:-}"
# Pinned by digest: the forwarder runs --network host, so a mutable tag is a supply-chain hole. Keep in sync with Makefile E2E_PROXY_FORWARDER_IMAGE and docker/Dockerfile.e2e's base.
PROXY_FORWARDER_IMAGE="${PROXY_FORWARDER_IMAGE:-debian:bookworm-slim@sha256:60eac759739651111db372c07be67863818726f754804b8707c90979bda511df}"
E2E_PROXY_FORWARDER_NAME="${E2E_PROXY_FORWARDER_NAME:-calm-e2e-proxy-forwarder}"
E2E_PROXY_SOCK_DIR="${E2E_PROXY_SOCK_DIR:-/tmp/calm-e2e-proxy}"
E2E_IMAGE_TAG="${E2E_IMAGE_TAG:-calm-e2e:bookworm}"
E2E_TIMEOUT="${E2E_TIMEOUT:-1500}"
DECOYS="${DECOYS:-0}"

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
CODEX_BIN_RAW="${NEIGE_CODEX_BIN:-$HOME/.local/bin/codex}"
AUTH_RAW="$HOME/.codex/auth.json"
KILLER_LOG=/home/kenji/neige-killer.log
PROXY_BIN="$TARGET_DIR/debug/e2e-egress-proxy"
# Host-compiled workspace bin, bind-mounted onto the run container's PATH so the planner agent's bare `neige` calls resolve.
NEIGE_BIN="$TARGET_DIR/debug/neige"

CONTAINER_HOME=/home/e2e
CODEX_MOUNT=/opt/codex/codex
RUN_NAME="calm-e2e-run-$$"
PREFLIGHT_NAME="calm-e2e-preflight-$$"

DRY_RUN=0
NO_BUILD=0
FORWARDER_ONLY=0
FORWARDER_DOWN=0
PREFLIGHT_ONLY=0
TEST_FILTER=""
TEST_BIN=""
# Presence flags: `--test ''` / `--test-bin ''` must still count as "flag
# given" for the lifecycle-mode rejection below (nonempty checks would let
# an explicit empty value slip past).
TEST_FILTER_SET=0
TEST_BIN_SET=0

log() { printf '[e2e-isolated] %s\n' "$*" >&2; }
die() { log "FATAL: $*"; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --no-build) NO_BUILD=1 ;;
        --forwarder-only) FORWARDER_ONLY=1 ;;
        --forwarder-down) FORWARDER_DOWN=1 ;;
        --preflight-only) PREFLIGHT_ONLY=1 ;;
        --test)
            [ $# -ge 2 ] || die "--test needs a value"
            TEST_FILTER="$2"; TEST_FILTER_SET=1; shift ;;
        --test-bin)
            [ $# -ge 2 ] || die "--test-bin needs a value"
            TEST_BIN="$2"; TEST_BIN_SET=1; shift ;;
        # Help = the header block: from line 2 to the closing `# ===` fence.
        -h|--help) sed -n '2,/^# ====/p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown flag: $1 (see --help)" ;;
    esac
    shift
done

if [ "$FORWARDER_ONLY" = 1 ] && [ "$FORWARDER_DOWN" = 1 ]; then
    die "--forwarder-only and --forwarder-down are mutually exclusive lifecycle modes"
fi
if [ "$FORWARDER_ONLY" = 1 ] || [ "$FORWARDER_DOWN" = 1 ]; then
    LIFECYCLE_MODE="--forwarder-only"
    if [ "$FORWARDER_DOWN" = 1 ]; then LIFECYCLE_MODE="--forwarder-down"; fi
    if [ "$DRY_RUN" = 1 ]; then
        die "$LIFECYCLE_MODE is a lifecycle mode: --dry-run does not apply (dry-run previews the run-container argv only)"
    fi
    if [ "$PREFLIGHT_ONLY" = 1 ]; then
        die "$LIFECYCLE_MODE is a lifecycle mode: --preflight-only does not apply (preflight belongs to an execution run)"
    fi
    if [ "$TEST_FILTER_SET" = 1 ]; then
        die "$LIFECYCLE_MODE is a lifecycle mode: --test does not apply (no suite runs in this mode)"
    fi
    if [ "$TEST_BIN_SET" = 1 ]; then
        die "$LIFECYCLE_MODE is a lifecycle mode: --test-bin does not apply (no suite runs in this mode)"
    fi
    if [ "$NO_BUILD" = 1 ]; then
        die "$LIFECYCLE_MODE is a lifecycle mode: --no-build does not apply (this mode never builds)"
    fi
    if [ "$DECOYS" != 0 ]; then
        die "$LIFECYCLE_MODE is a lifecycle mode: DECOYS=$DECOYS does not apply (decoys ride the execution run)"
    fi
fi
if [ "$DRY_RUN" = 1 ] && [ "$PREFLIGHT_ONLY" = 1 ]; then
    die "--dry-run and --preflight-only conflict: dry-run prints and executes nothing, preflight-only executes the fence check"
fi
if [ "$FORWARDER_DOWN" != 1 ] && [ -z "$CALM_HOST_PROXY_PORT" ]; then
    die "CALM_HOST_PROXY_PORT is empty — the container has no other egress; set it (host .env) or export it"
fi

resolve() {
    local p="$1" r
    if r="$(readlink -f -- "$p" 2>/dev/null)"; then
        printf '%s' "$r"
    elif [ "$DRY_RUN" = 1 ]; then
        printf '%s' "$p"
    else
        die "path does not exist: $p"
    fi
}

HOST_UID="$(id -u)"
HOST_GID="$(id -g)"

# Deferred: the forwarder lifecycle modes must work without codex/auth credentials.
CODEX_REAL=""
AUTH_REAL=""
resolve_inputs() {
    CODEX_REAL="$(resolve "$CODEX_BIN_RAW")"
    AUTH_REAL="$(resolve "$AUTH_RAW")"
}

ensure_proxy_bin() {
    if [ "$NO_BUILD" = 1 ] && [ -x "$PROXY_BIN" ]; then
        log "reusing existing egress proxy binary: $PROXY_BIN"
        return 0
    fi
    log "host-compiling egress proxy (cargo build -p e2e-egress-proxy) ..."
    RUSTC_WRAPPER='' CARGO_BUILD_JOBS=4 nice -n 10 \
        cargo build --manifest-path "$REPO_ROOT/Cargo.toml" \
        -p e2e-egress-proxy --bin e2e-egress-proxy
    [ -x "$PROXY_BIN" ] || die "egress proxy binary missing after build at $PROXY_BIN"
}

# Shared singleton forwarder: torn down only by --forwarder-down, never by a run's trap.
ensure_forwarder() {
    ensure_proxy_bin
    local sock="$E2E_PROXY_SOCK_DIR/proxy.sock"
    # Lockfile lives in the sock dir's PARENT so teardown can rm -rf the dir while holding the lock; it is never unlinked (split-brain hazard).
    local lock="${E2E_PROXY_SOCK_DIR%/}.lock"
    # bin= pins which gate binary backs the forwarder; after rebuilding the proxy you must --forwarder-down to pick it up.
    local spec="egress-proxy:$sock->$CALM_HOST_PROXY_HOST:$CALM_HOST_PROXY_PORT bin=$PROXY_BIN image=$PROXY_FORWARDER_IMAGE"
    # All shared sock-dir mutations happen only under this lock so a concurrent teardown's rm -rf never interleaves with our mkdir/chmod.
    (
        flock -w 60 9 || { log "FATAL: could not acquire forwarder lock $lock within 60s"; exit 1; }
        mkdir -p "$E2E_PROXY_SOCK_DIR"
        chmod 700 "$E2E_PROXY_SOCK_DIR"
        if docker inspect "$E2E_PROXY_FORWARDER_NAME" >/dev/null 2>&1; then
            local existing running
            existing="$(docker inspect -f '{{index .Config.Labels "calm.proxy.spec"}}' "$E2E_PROXY_FORWARDER_NAME" 2>/dev/null || echo "")"
            running="$(docker inspect -f '{{.State.Running}}' "$E2E_PROXY_FORWARDER_NAME" 2>/dev/null || echo false)"
            if [ "$existing" != "$spec" ]; then
                log "FATAL: forwarder '$E2E_PROXY_FORWARDER_NAME' exists with a DIFFERENT config:"
                log "  existing: ${existing:-<no proxy label>}"
                log "  wanted:   $spec"
                log "refusing to recreate (a concurrent run may depend on it)."
                log "if no isolated e2e run is live, run: make e2e-proxy-forwarder-down   (or scripts/e2e-isolated/run.sh --forwarder-down), then retry."
                exit 1
            elif [ "$running" != "true" ]; then
                docker start "$E2E_PROXY_FORWARDER_NAME" >/dev/null
                log "forwarder restarted: $spec"
            else
                log "forwarder already up: $spec"
            fi
        fi
        if ! docker inspect "$E2E_PROXY_FORWARDER_NAME" >/dev/null 2>&1; then
            # --network host so the gate dials the host-loopback sing-box; it publishes no ports, its only listener is the unix socket (mode 600, our uid).
            docker run -d --network host \
                --name "$E2E_PROXY_FORWARDER_NAME" \
                --user "$HOST_UID:$HOST_GID" \
                --label "calm.proxy.spec=$spec" \
                --restart unless-stopped \
                -v "$E2E_PROXY_SOCK_DIR:/sock" \
                -v "$PROXY_BIN:/usr/local/bin/e2e-egress-proxy:ro" \
                "$PROXY_FORWARDER_IMAGE" \
                /usr/local/bin/e2e-egress-proxy /sock/proxy.sock \
                "$CALM_HOST_PROXY_HOST:$CALM_HOST_PROXY_PORT" >/dev/null
            log "forwarder created: $spec"
        fi
    ) 9>"$lock"
    for _ in $(seq 1 50); do
        [ -S "$sock" ] && return 0
        sleep 0.2
    done
    die "forwarder socket never appeared at $sock"
}

forwarder_down() {
    # Guarded teardown: canonicalize the dir and require the owned prefix so
    # a mis-set E2E_PROXY_SOCK_DIR can never turn the rm -rf destructive.
    # The prefix check also structurally excludes "" and "/".
    local dir lock
    dir="$(readlink -f -- "$E2E_PROXY_SOCK_DIR" 2>/dev/null || true)"
    case "$dir" in
        /tmp/calm-e2e-proxy*) : ;;
        *) die "refusing teardown: E2E_PROXY_SOCK_DIR='$E2E_PROXY_SOCK_DIR' canonicalizes to '${dir:-<unresolvable>}', outside the owned prefix /tmp/calm-e2e-proxy*" ;;
    esac
    lock="${dir%/}.lock"
    (
        flock -w 60 9 || { log "FATAL: could not acquire forwarder lock $lock within 60s"; exit 1; }
        if docker rm -f "$E2E_PROXY_FORWARDER_NAME" >/dev/null 2>&1; then
            log "e2e forwarder removed: $E2E_PROXY_FORWARDER_NAME"
        else
            log "e2e forwarder not present: $E2E_PROXY_FORWARDER_NAME"
        fi
        rm -rf -- "$dir"
        log "socket dir removed: $dir"
    ) 9>"$lock"
}

build_test_bin() {
    log "host-compiling test binary (cargo test --no-run) ..."
    local json
    json="$(mktemp)"
    RUSTC_WRAPPER='' CARGO_BUILD_JOBS=4 nice -n 10 \
        cargo test --manifest-path "$REPO_ROOT/Cargo.toml" -p calm-server \
        --features codex-e2e,fixtures --test codex_forge_e2e --no-run \
        --message-format=json >"$json"
    # `|| true`: a no-match grep must fall through to the explicit die below
    # (under pipefail the bare pipeline would kill the script wordlessly).
    TEST_BIN="$(grep -o '"executable":"[^"]*/codex_forge_e2e-[^"]*"' "$json" | tail -1 | cut -d'"' -f4 || true)"
    rm -f "$json"
    [ -n "$TEST_BIN" ] || die "could not parse test executable from cargo JSON output"
    log "building neige-mcp-stdio-shim (target-dir sibling the binary execs) ..."
    RUSTC_WRAPPER='' CARGO_BUILD_JOBS=4 nice -n 10 \
        cargo build --manifest-path "$REPO_ROOT/Cargo.toml" \
        -p neige-mcp-stdio-shim --bin neige-mcp-stdio-shim
    ensure_neige_bin
}

ensure_neige_bin() {
    log "building neige CLI (agent shells out bare \`neige\` for track reads) ..."
    RUSTC_WRAPPER='' CARGO_BUILD_JOBS=4 nice -n 10 \
        cargo build --manifest-path "$REPO_ROOT/Cargo.toml" \
        -p neige-cli --bin neige
    [ -x "$NEIGE_BIN" ] || die "neige CLI binary missing after build at $NEIGE_BIN"
}

discover_test_bin() {
    local f newest=""
    for f in "$TARGET_DIR"/debug/deps/codex_forge_e2e-*; do
        [[ "$f" == *.d ]] && continue
        [ -f "$f" ] || continue
        if [ -z "$newest" ] || [ "$f" -nt "$newest" ]; then
            newest="$f"
        fi
    done
    if [ -n "$newest" ]; then
        TEST_BIN="$newest"
    elif [ "$DRY_RUN" = 1 ]; then
        TEST_BIN="$TARGET_DIR/debug/deps/codex_forge_e2e-UNBUILT"
    else
        die "no built codex_forge_e2e binary under $TARGET_DIR/debug/deps (run without --no-build)"
    fi
}

docker_run_args() {
    local mode="$1" name="$2"
    DOCKER_ARGS=(
        --name "$name"
        --network none
        --user "$HOST_UID:$HOST_GID"
        --security-opt seccomp=unconfined
        --security-opt apparmor=unconfined
        --memory=24g --memory-swap=24g --cpus=8 --pids-limit=6000
        --init --rm
        -v "$REPO_ROOT:$REPO_ROOT:ro"
        -v "$TARGET_DIR:$TARGET_DIR:ro"
        -v "$CODEX_REAL:$CODEX_MOUNT:ro"
        -v "$AUTH_REAL:$CONTAINER_HOME/.codex/auth.json:ro"
        -v "$NEIGE_BIN:/usr/local/bin/neige:ro"
        # ro: connect(2) to a unix socket works on a read-only mount; agents must not scribble in the host dir.
        -v "$E2E_PROXY_SOCK_DIR:/sock:ro"
        # exec: docker tmpfs defaults to noexec, but agent workspaces live
        # under $HOME/.cache and must exec what they write (gates, hooks).
        --tmpfs "$CONTAINER_HOME:rw,exec,uid=$HOST_UID,gid=$HOST_GID,mode=700"
        --workdir "$REPO_ROOT/crates/calm-server"
        -e "HOME=$CONTAINER_HOME"
        -e "NEIGE_CODEX_BIN=$CODEX_MOUNT"
        -e "NEIGE_CODEX_PROXY=http://127.0.0.1:2081"
        -e "NO_PROXY=127.0.0.1,localhost"
        -e "no_proxy=127.0.0.1,localhost"
        -e "RUST_BACKTRACE=1"
        -e "E2E_MODE=$mode"
        -e "E2E_TEST_BIN=$TEST_BIN"
        -e "E2E_TEST_FILTER=$TEST_FILTER"
        -e "DECOYS=$DECOYS"
    )
    # Forwarded only when set+non-empty so the default argv (and the check_dry_run.sh golden) stays byte-identical.
    if [ -n "${NEIGE_PLANNER_PLANNING_BUDGET:-}" ]; then
        DOCKER_ARGS+=(-e "NEIGE_PLANNER_PLANNING_BUDGET=$NEIGE_PLANNER_PLANNING_BUDGET")
    fi
    if [ -n "${NEIGE_CODEX_FORGE_E2E_BUDGET:-}" ]; then
        DOCKER_ARGS+=(-e "NEIGE_CODEX_FORGE_E2E_BUDGET=$NEIGE_CODEX_FORGE_E2E_BUDGET")
    fi
    # Image + command MUST stay last: check_dry_run.sh asserts the argv ends at
    # entry.sh, and any docker flag placed after the image would be parsed as a
    # command arg rather than a `docker run` flag.
    DOCKER_ARGS+=(
        "$E2E_IMAGE_TAG"
        bash "$REPO_ROOT/scripts/e2e-isolated/entry.sh"
    )
}

print_argv() {
    printf 'docker run'
    printf ' %q' "$@"
    printf '\n'
}

if [ "$FORWARDER_DOWN" = 1 ]; then
    forwarder_down
    exit 0
fi

if [ "$FORWARDER_ONLY" = 1 ]; then
    ensure_forwarder
    exit 0
fi

resolve_inputs

if [ -z "$TEST_BIN" ]; then
    if [ "$DRY_RUN" = 1 ] || [ "$NO_BUILD" = 1 ]; then
        discover_test_bin
    else
        build_test_bin
    fi
fi

if [ "$DRY_RUN" = 1 ]; then
    # Print everything, execute NOTHING (no docker daemon, no cargo, no
    # state change of any kind — not even mkdir).
    docker_run_args run "$RUN_NAME"
    echo "--- dry-run: resolved inputs ---"
    echo "repo (ro mount)        : $REPO_ROOT"
    echo "cargo target (ro mount): $TARGET_DIR"
    echo "test binary            : $TEST_BIN"
    echo "codex (ro file mount)  : $CODEX_REAL -> $CODEX_MOUNT"
    echo "neige CLI (ro file)    : $NEIGE_BIN -> /usr/local/bin/neige"
    echo "auth.json (ro file)    : $AUTH_REAL -> $CONTAINER_HOME/.codex/auth.json"
    echo "forwarder socket dir   : $E2E_PROXY_SOCK_DIR (ro mount at /sock)"
    echo "egress gate (forwarder): $PROXY_BIN -> /sock/proxy.sock (bind-mounted into $E2E_PROXY_FORWARDER_NAME, $PROXY_FORWARDER_IMAGE)"
    echo "upstream proxy         : $CALM_HOST_PROXY_HOST:$CALM_HOST_PROXY_PORT (sing-box, dialed by the gate for admitted CONNECTs only)"
    echo "timeout                : ${E2E_TIMEOUT}s; EXIT trap removes only $RUN_NAME"
    echo "--- dry-run: docker run argv (run container) ---"
    print_argv "${DOCKER_ARGS[@]}"
    echo "--- dry-run: end argv ---"
    exit 0
fi

[ -x "$TEST_BIN" ] || die "test binary not executable: $TEST_BIN"
[ -x "$TARGET_DIR/debug/neige-mcp-stdio-shim" ] || die "neige-mcp-stdio-shim missing beside the test binary (build it first)"
# Bind-mounting a missing source silently creates a directory in the container, so fail loud here instead.
[ -x "$NEIGE_BIN" ] || die "neige CLI missing at $NEIGE_BIN (build it first, or drop --no-build)"
[ -f "$AUTH_REAL" ] || die "codex auth.json not found at $AUTH_REAL"
[ -x "$CODEX_REAL" ] || die "codex binary not found/executable at $CODEX_REAL"

KILLER_SNAP=""
# shellcheck disable=SC2317,SC2329  # invoked via the EXIT trap only
cleanup() {
    # ONLY the per-run containers. NEVER the shared forwarder (a concurrent
    # run's egress would be cut) — it has its own explicit down mode.
    docker rm -f "$RUN_NAME" >/dev/null 2>&1 || true
    docker rm -f "$PREFLIGHT_NAME" >/dev/null 2>&1 || true
    if [ -n "$KILLER_SNAP" ]; then
        rm -f -- "$KILLER_SNAP"
    fi
}
trap cleanup EXIT

# Snapshot only `sig=` capture records: a broken bpftrace probe appends compile-error spam to the same file, so a whole-file diff would false-alarm.
if [ -r "$KILLER_LOG" ]; then
    KILLER_SNAP="$(mktemp)"
    grep -a 'sig=' -- "$KILLER_LOG" >"$KILLER_SNAP" 2>/dev/null || true
    log "killer-log snapshot: $(wc -l <"$KILLER_SNAP") sig= capture record(s) baseline"
else
    log "NOTICE: killer log $KILLER_LOG missing/unreadable — the post-run kill-forensics diff will be SKIPPED"
fi

log "building image $E2E_IMAGE_TAG ..."
# Build-time networking only (apt); the run container stays --network none.
BUILD_PROXY="${http_proxy:-http://$CALM_HOST_PROXY_HOST:$CALM_HOST_PROXY_PORT}"
docker build --network host -f "$REPO_ROOT/docker/Dockerfile.e2e" -t "$E2E_IMAGE_TAG" \
    --build-arg "http_proxy=$BUILD_PROXY" \
    --build-arg "https_proxy=${https_proxy:-$BUILD_PROXY}" \
    --build-arg "no_proxy=${no_proxy:-}" \
    "$REPO_ROOT/docker" >/dev/null
ensure_forwarder

log "preflight: fence + exec probe (container $PREFLIGHT_NAME) ..."
docker_run_args preflight "$PREFLIGHT_NAME"
timeout 180 docker run "${DOCKER_ARGS[@]}" \
    || die "preflight failed — fence breach, dead chain, or exec probe failure; NOT running codex"
log "preflight OK: chain live, prod unreachable through it; binary executes in-image"

if [ "$PREFLIGHT_ONLY" = 1 ]; then
    log "--preflight-only: stopping before any codex runs"
    exit 0
fi

# create (not run) first so isolation is asserted from the outside before a single process starts.
docker_run_args run "$RUN_NAME"
docker create "${DOCKER_ARGS[@]}" >/dev/null

NETMODE="$(docker inspect -f '{{.HostConfig.NetworkMode}}' "$RUN_NAME")"
PIDMODE="$(docker inspect -f '{{.HostConfig.PidMode}}' "$RUN_NAME")"
PRIVILEGED="$(docker inspect -f '{{.HostConfig.Privileged}}' "$RUN_NAME")"
[ "$NETMODE" = "none" ] || die "container NetworkMode=$NETMODE (expected none)"
case "$PIDMODE" in
    ""|private) : ;;  # both spellings mean an isolated PID namespace
    *) die "container PidMode=$PIDMODE (expected private)" ;;
esac
[ "$PRIVILEGED" = "false" ] || die "container is privileged"
log "inspect OK: network=none pid=private privileged=false"

log "running suite (timeout ${E2E_TIMEOUT}s; container $RUN_NAME) ..."
set +e
timeout "$E2E_TIMEOUT" docker start -a "$RUN_NAME"
RC=$?
set -e
if [ "$RC" -eq 124 ]; then
    log "TIMED OUT after ${E2E_TIMEOUT}s — container will be force-removed"
fi

if [ -n "$KILLER_SNAP" ] && [ -r "$KILLER_LOG" ]; then
    KILLER_NOW="$(mktemp)"
    grep -a 'sig=' -- "$KILLER_LOG" >"$KILLER_NOW" 2>/dev/null || true
    NEW_SIG="$(diff -- "$KILLER_SNAP" "$KILLER_NOW" 2>/dev/null | grep -c '^>' || true)"
    if [ "${NEW_SIG:-0}" -eq 0 ]; then
        log "killer-log diff: no new sig= capture records (no prod kills observed)"
    else
        log "killer-log gained $NEW_SIG new sig= capture record(s) during the run — REAL KILL SIGNAL, READ IT:"
        diff -- "$KILLER_SNAP" "$KILLER_NOW" 2>/dev/null | grep '^>' >&2 || true
        [ "$RC" -eq 0 ] && RC=96
    fi
    rm -f -- "$KILLER_NOW"
fi

log "exit status: $RC"
exit "$RC"
