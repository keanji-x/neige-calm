#!/usr/bin/env bash
# =============================================================================
# #863 — Docker-isolated codex-e2e tier runner.
#
# Runs the calm-server `codex_forge_e2e` suite fully contained in Docker so a
# buggy/overeager real agent can never touch host prod processes again
# (proven killer: name-based kills from inside the suite hit prod
# neige-app/calm-server — see /home/kenji/neige-killer.log and issue #863).
# Design doc: #863 "Docker-isolated codex-e2e tier" (docs/_863-*-design.md
# while in review; authoritative content posted on the issue).
#
# Model (host-compile, run-in-container):
#   1. Host builds the test binary with the warm shared target
#      (`cargo test --no-run --features codex-e2e,fixtures`) + the sibling
#      neige-mcp-stdio-shim. Host glibc == bookworm-slim glibc (Debian 12).
#   2. The repo checkout and CARGO_TARGET_DIR are bind-mounted READ-ONLY at
#      their identical host paths (the binary bakes CARGO_MANIFEST_DIR and
#      locates the shim as a target-dir sibling). The resolved codex CLI
#      (readlink -f, never the ~/.codex symlink tree) is mounted as a single
#      ro file at /opt/codex/codex; host ~/.codex/auth.json is the ONLY other
#      credential mounted (single file, ro — #897 keeps the rest out).
#   3. The run container gets `--network none`: no IP path to prod
#      :4040/:4041 by construction. Its only egress is loopback :2081 →
#      (in-container socat) → /sock/proxy.sock → (host forwarder container,
#      singleton `calm-e2e-proxy-forwarder`) → the host proxy
#      CALM_HOST_PROXY_HOST:CALM_HOST_PROXY_PORT (sing-box).
#   4. REQUIRED fence preflight before any codex runs: an HTTP GET to prod
#      127.0.0.1:4040 and :4041 THROUGH that full chain must fail
#      (timeout/refused/proxy 5xx are fine). Any origin-looking HTTP answer —
#      especially a 200 with a version string — means sing-box routing would
#      hand agents a path to prod: ABORT. The fence rests on sing-box config,
#      so it is asserted every run, never assumed (design §B).
#   5. Rails (proven scope values): --memory=24g --memory-swap=24g (no swap)
#      --cpus=8 --pids-limit=6000, non-root --user, seccomp+apparmor
#      unconfined (needed for codex's bwrap userns; NO SYS_ADMIN — verified
#      sufficient on this box), --init, timeout 1500s, EXIT trap removes ONLY
#      the per-run container (never the shared forwarder).
#
# Usage:
#   scripts/e2e-isolated/run.sh                      # whole suite
#   scripts/e2e-isolated/run.sh --test NAME          # one test (--exact)
#   scripts/e2e-isolated/run.sh --dry-run            # print argv, run nothing
#   scripts/e2e-isolated/run.sh --preflight-only     # build+image+forwarder+
#                                                    # fence + `--list` probe,
#                                                    # no codex, then stop
#   scripts/e2e-isolated/run.sh --forwarder-only     # ensure forwarder, stop
#   scripts/e2e-isolated/run.sh --no-build           # reuse existing binary
#   scripts/e2e-isolated/run.sh --test-bin PATH      # explicit test binary
#   DECOYS=1 scripts/e2e-isolated/run.sh             # plant name-decoy
#                                                    # processes, assert they
#                                                    # survive (regression
#                                                    # telemetry, design §F)
#
# Make wrappers: `make e2e-codex-isolated` / `e2e-proxy-forwarder-up|down` /
# `e2e-codex-isolated-check` (shellcheck + dry-run golden, no docker needed).
#
# Smoke protocol: the first REAL run must follow the design's §6 checklist
# (killer-log baseline, prod pid snapshot, setsid-detached launch — real e2e
# crashes a harness-tracked shell, ONE smoke test with DECOYS=1 before any
# full-suite run, post-run killer-log/prod/ro-mount audit). The bpftrace
# forensic probe stays a manual root tool; it is NOT wrapped here.
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"

# ---- configuration (Makefile variables / flags; no new implicit env knobs —
# CARGO_TARGET_DIR and NEIGE_CODEX_BIN are pre-existing seams) ---------------
CALM_HOST_PROXY_HOST="${CALM_HOST_PROXY_HOST:-127.0.0.1}"
CALM_HOST_PROXY_PORT="${CALM_HOST_PROXY_PORT:-2080}"
PROXY_FORWARDER_IMAGE="${PROXY_FORWARDER_IMAGE:-alpine/socat}"
E2E_PROXY_FORWARDER_NAME="${E2E_PROXY_FORWARDER_NAME:-calm-e2e-proxy-forwarder}"
E2E_PROXY_SOCK_DIR="${E2E_PROXY_SOCK_DIR:-/tmp/calm-e2e-proxy}"
E2E_IMAGE_TAG="${E2E_IMAGE_TAG:-calm-e2e:bookworm}"
E2E_TIMEOUT="${E2E_TIMEOUT:-1500}"
DECOYS="${DECOYS:-0}"

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
CODEX_BIN_RAW="${NEIGE_CODEX_BIN:-$HOME/.local/bin/codex}"
AUTH_RAW="$HOME/.codex/auth.json"
KILLER_LOG=/home/kenji/neige-killer.log

CONTAINER_HOME=/home/e2e
CODEX_MOUNT=/opt/codex/codex
RUN_NAME="calm-e2e-run-$$"
PREFLIGHT_NAME="calm-e2e-preflight-$$"

DRY_RUN=0
NO_BUILD=0
FORWARDER_ONLY=0
PREFLIGHT_ONLY=0
TEST_FILTER=""
TEST_BIN=""

log() { printf '[e2e-isolated] %s\n' "$*" >&2; }
die() { log "FATAL: $*"; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --no-build) NO_BUILD=1 ;;
        --forwarder-only) FORWARDER_ONLY=1 ;;
        --preflight-only) PREFLIGHT_ONLY=1 ;;
        --test)
            [ $# -ge 2 ] || die "--test needs a value"
            TEST_FILTER="$2"; shift ;;
        --test-bin)
            [ $# -ge 2 ] || die "--test-bin needs a value"
            TEST_BIN="$2"; shift ;;
        -h|--help) sed -n '2,66p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) die "unknown flag: $1 (see --help)" ;;
    esac
    shift
done

[ -n "$CALM_HOST_PROXY_PORT" ] || die "CALM_HOST_PROXY_PORT is empty — the \
container has no other egress; set it (host .env) or export it"

resolve() {
    # readlink -f, but tolerant of missing paths in --dry-run mode.
    local p="$1" r
    if r="$(readlink -f -- "$p" 2>/dev/null)"; then
        printf '%s' "$r"
    elif [ "$DRY_RUN" = 1 ]; then
        printf '%s' "$p"
    else
        die "path does not exist: $p"
    fi
}

CODEX_REAL="$(resolve "$CODEX_BIN_RAW")"
AUTH_REAL="$(resolve "$AUTH_RAW")"
HOST_UID="$(id -u)"
HOST_GID="$(id -g)"

# ---- host forwarder (shared singleton — mirrors Makefile proxy-forwarder-up;
# torn down ONLY by `make e2e-proxy-forwarder-down`, never by a run's trap) --
ensure_forwarder() {
    local sock="$E2E_PROXY_SOCK_DIR/proxy.sock"
    local spec="unix:$sock->$CALM_HOST_PROXY_HOST:$CALM_HOST_PROXY_PORT"
    mkdir -p "$E2E_PROXY_SOCK_DIR"
    chmod 700 "$E2E_PROXY_SOCK_DIR"
    if docker inspect "$E2E_PROXY_FORWARDER_NAME" >/dev/null 2>&1; then
        local existing running
        existing="$(docker inspect -f '{{index .Config.Labels "calm.proxy.spec"}}' "$E2E_PROXY_FORWARDER_NAME" 2>/dev/null || echo "")"
        running="$(docker inspect -f '{{.State.Running}}' "$E2E_PROXY_FORWARDER_NAME" 2>/dev/null || echo false)"
        if [ "$existing" != "$spec" ]; then
            log "forwarder spec changed ($existing -> $spec); recreating"
            docker rm -f "$E2E_PROXY_FORWARDER_NAME" >/dev/null
        elif [ "$running" != "true" ]; then
            docker start "$E2E_PROXY_FORWARDER_NAME" >/dev/null
            log "forwarder restarted: $spec"
        else
            log "forwarder already up: $spec"
        fi
    fi
    if ! docker inspect "$E2E_PROXY_FORWARDER_NAME" >/dev/null 2>&1; then
        # --network host: its 127.0.0.1 is the host's, so it can reach the
        # host-loopback proxy. It publishes NO ports; its only listener is
        # the unix socket in E2E_PROXY_SOCK_DIR (mode 600, our uid).
        docker run -d --network host \
            --name "$E2E_PROXY_FORWARDER_NAME" \
            --user "$HOST_UID:$HOST_GID" \
            --label "calm.proxy.spec=$spec" \
            --restart unless-stopped \
            -v "$E2E_PROXY_SOCK_DIR:/sock" \
            "$PROXY_FORWARDER_IMAGE" \
            "UNIX-LISTEN:/sock/proxy.sock,fork,mode=600,unlink-early" \
            "TCP:$CALM_HOST_PROXY_HOST:$CALM_HOST_PROXY_PORT" >/dev/null
        log "forwarder created: $spec"
    fi
    for _ in $(seq 1 50); do
        [ -S "$sock" ] && return 0
        sleep 0.2
    done
    die "forwarder socket never appeared at $sock"
}

# ---- test binary --------------------------------------------------------
build_test_bin() {
    log "host-compiling test binary (cargo test --no-run) ..."
    local json
    json="$(mktemp)"
    RUSTC_WRAPPER='' CARGO_BUILD_JOBS=4 nice -n 10 \
        cargo test --manifest-path "$REPO_ROOT/Cargo.toml" -p calm-server \
        --features codex-e2e,fixtures --test codex_forge_e2e --no-run \
        --message-format=json >"$json"
    TEST_BIN="$(grep -o '"executable":"[^"]*/codex_forge_e2e-[^"]*"' "$json" | tail -1 | cut -d'"' -f4)"
    rm -f "$json"
    [ -n "$TEST_BIN" ] || die "could not parse test executable from cargo JSON output"
    log "building neige-mcp-stdio-shim (target-dir sibling the binary execs) ..."
    RUSTC_WRAPPER='' CARGO_BUILD_JOBS=4 nice -n 10 \
        cargo build --manifest-path "$REPO_ROOT/Cargo.toml" \
        -p neige-mcp-stdio-shim --bin neige-mcp-stdio-shim
}

discover_test_bin() {
    # Newest already-built binary (for --no-build / --dry-run without cargo).
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

# ---- docker run argv (single source for dry-run print and real run) -----
# E2E_MODE=preflight makes entry.sh stop after the fence check + a
# `--list` exec probe (glibc/mount-layout proof); E2E_MODE=run executes
# the suite. Everything security-relevant is identical between the two.
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
        -v "$E2E_PROXY_SOCK_DIR:/sock"
        --tmpfs "$CONTAINER_HOME:rw,uid=$HOST_UID,gid=$HOST_GID,mode=700"
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
        "$E2E_IMAGE_TAG"
        bash "$REPO_ROOT/scripts/e2e-isolated/entry.sh"
    )
}

print_argv() {
    printf 'docker run'
    printf ' %q' "$@"
    printf '\n'
}

# =========================================================================
if [ "$FORWARDER_ONLY" = 1 ]; then
    ensure_forwarder
    exit 0
fi

if [ -z "$TEST_BIN" ]; then
    if [ "$DRY_RUN" = 1 ] || [ "$NO_BUILD" = 1 ]; then
        discover_test_bin
    else
        build_test_bin
    fi
fi

if [ "$DRY_RUN" = 1 ]; then
    # Print everything, execute NOTHING (no docker daemon, no cargo needed).
    docker_run_args run "$RUN_NAME"
    echo "--- dry-run: resolved inputs ---"
    echo "repo (ro mount)        : $REPO_ROOT"
    echo "cargo target (ro mount): $TARGET_DIR"
    echo "test binary            : $TEST_BIN"
    echo "codex (ro file mount)  : $CODEX_REAL -> $CODEX_MOUNT"
    echo "auth.json (ro file)    : $AUTH_REAL -> $CONTAINER_HOME/.codex/auth.json"
    echo "forwarder socket dir   : $E2E_PROXY_SOCK_DIR (rw mount at /sock)"
    echo "upstream proxy         : $CALM_HOST_PROXY_HOST:$CALM_HOST_PROXY_PORT (via $E2E_PROXY_FORWARDER_NAME)"
    echo "timeout                : ${E2E_TIMEOUT}s; EXIT trap removes only $RUN_NAME"
    echo "--- dry-run: docker run argv (run container) ---"
    print_argv "${DOCKER_ARGS[@]}"
    echo "--- dry-run: end argv ---"
    exit 0
fi

[ -x "$TEST_BIN" ] || die "test binary not executable: $TEST_BIN"
[ -x "$TARGET_DIR/debug/neige-mcp-stdio-shim" ] || die "neige-mcp-stdio-shim missing beside the test binary (build it first)"
[ -f "$AUTH_REAL" ] || die "codex auth.json not found at $AUTH_REAL"
[ -x "$CODEX_REAL" ] || die "codex binary not found/executable at $CODEX_REAL"

# ---- killer-log baseline snapshot (design §5/§6) -------------------------
KILLER_SNAP=""
if [ -r "$KILLER_LOG" ]; then
    KILLER_SNAP="$(mktemp)"
    cp -- "$KILLER_LOG" "$KILLER_SNAP"
    log "killer-log snapshot: $(wc -l <"$KILLER_SNAP") lines baseline"
fi

log "building image $E2E_IMAGE_TAG ..."
# Build-time networking only (apt fetching Debian packages) — the RUN
# container remains --network none. --network host lets apt use the
# host-loopback proxy on this box; explicit http_proxy env still wins.
BUILD_PROXY="${http_proxy:-http://$CALM_HOST_PROXY_HOST:$CALM_HOST_PROXY_PORT}"
docker build --network host -f "$REPO_ROOT/docker/Dockerfile.e2e" -t "$E2E_IMAGE_TAG" \
    --build-arg "http_proxy=$BUILD_PROXY" \
    --build-arg "https_proxy=${https_proxy:-$BUILD_PROXY}" \
    --build-arg "no_proxy=${no_proxy:-}" \
    "$REPO_ROOT/docker" >/dev/null
ensure_forwarder

# shellcheck disable=SC2317  # invoked via the EXIT trap only
cleanup() {
    # ONLY the per-run containers. NEVER the shared forwarder (a concurrent
    # run's egress would be cut) — it has its own explicit down target.
    docker rm -f "$RUN_NAME" >/dev/null 2>&1 || true
    docker rm -f "$PREFLIGHT_NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# ---- preflight container: fence check + `--list` exec probe --------------
# Same argv/mounts/posture as the real run; entry.sh in preflight mode
# asserts the proxy chain CANNOT reach prod :4040/:4041, then proves the
# host-built binary executes in-image (glibc/layout) via `--list`.
log "preflight: fence + exec probe (container $PREFLIGHT_NAME) ..."
docker_run_args preflight "$PREFLIGHT_NAME"
timeout 120 docker run "${DOCKER_ARGS[@]}" \
    || die "preflight failed — fence breach or exec probe failure; NOT running codex"
log "preflight OK: prod unreachable through the chain; binary executes in-image"

if [ "$PREFLIGHT_ONLY" = 1 ]; then
    log "--preflight-only: stopping before any codex runs"
    exit 0
fi

# ---- the real run ---------------------------------------------------------
# create (not run) first so we can assert isolation from the OUTSIDE before
# a single process starts, then start attached under the timeout budget.
docker_run_args run "$RUN_NAME"
docker create "${DOCKER_ARGS[@]}" >/dev/null

NETMODE="$(docker inspect -f '{{.HostConfig.NetworkMode}}' "$RUN_NAME")"
PIDMODE="$(docker inspect -f '{{.HostConfig.PidMode}}' "$RUN_NAME")"
PRIVILEGED="$(docker inspect -f '{{.HostConfig.Privileged}}' "$RUN_NAME")"
[ "$NETMODE" = "none" ] || die "container NetworkMode=$NETMODE (expected none)"
[ -z "$PIDMODE" ] || die "container PidMode=$PIDMODE (expected private)"
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

# ---- post-run killer-log diff ---------------------------------------------
if [ -n "$KILLER_SNAP" ] && [ -r "$KILLER_LOG" ]; then
    if diff -q "$KILLER_SNAP" "$KILLER_LOG" >/dev/null 2>&1; then
        log "killer-log diff: EMPTY (no prod kills observed)"
    else
        log "killer-log CHANGED during the run — READ IT:"
        diff "$KILLER_SNAP" "$KILLER_LOG" >&2 || true
        [ "$RC" -eq 0 ] && RC=96
    fi
    rm -f "$KILLER_SNAP"
fi

log "exit status: $RC"
exit "$RC"
