#!/usr/bin/env bash
# Opt-in Tier 2 usability evidence; never run on the shared production host.
set -euo pipefail

if [[ ${1:-} != --dedicated-test-host || $# != 1 ]]; then
  printf 'Usage: %s --dedicated-test-host\nRequires a dedicated test host, authenticated Codex and Claude; spends tokens.\n' "$0" >&2
  exit 2
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR/.." rev-parse --show-toplevel)"
ENV_FILE="$REPO_ROOT/.env"
export NO_PROXY=127.0.0.1,localhost
export no_proxy="$NO_PROXY"
# shellcheck source=e2e/lib/assert.sh
source "$SCRIPT_DIR/lib/assert.sh"
# shellcheck source=e2e/lib/stack.sh
source "$SCRIPT_DIR/lib/stack.sh"
# shellcheck source=e2e/lib/api.sh
source "$SCRIPT_DIR/lib/api.sh"

command -v python3 >/dev/null || fail 'python3 is required'
stack_preflight 1
RUN_ID="planner-claude-$(date +%s)-$RANDOM"
DEV_ID="$RUN_ID"
PROJECT="neige-calm-$DEV_ID"
WORKSPACE="$E2E_CONTAINER_STATE_DIR/e2e-workspace"
ARTIFACT_DIR="$REPO_ROOT/e2e-artifacts/$RUN_ID"
source_sha="$(git -C "$REPO_ROOT" rev-parse HEAD)"
[[ -z "$(git -C "$REPO_ROOT" status --porcelain)" ]] || fail 'Commit the tested source before collecting UX evidence'
export NEIGE_BUILD_SHA="$source_sha"
COOKIE_HEADER=
SERVER_CID=
PORT="$(pick_port)"
umask 077
mkdir -p "$ARTIFACT_DIR"
# Only this freshly created compose project is torn down. Deliberately avoid
# dump_artifacts: raw server logs are not the sanitized UX transcript.
trap 'status=$?; trap - EXIT; teardown_stack; exit "$status"' EXIT
start_stack
wait_for_health
init_workspace
autologin_probe
login_unless_autologin "$AUTH_PROBE_STATUS"

# No install, login, auth-file reads, substitute executable or fallback model.
claude_bin="$(docker exec "$SERVER_CID" sh -lc 'command -v claude')" \
  || fail 'Claude is missing from the dedicated stack PATH; provision it first'
[[ "$claude_bin" == /* && "$claude_bin" != *$'\n'* ]] || fail 'Claude path must be absolute'
claude_version="$(docker exec "$SERVER_CID" "$claude_bin" --version)"
codex_version="$(docker exec "$SERVER_CID" codex --version)"

# Credentials stay in private stdin, never arguments or artifacts. The driver
# only sends ordinary Planner goals; it has no terminal-tool dispatch path.
printf '%s' "$COOKIE_HEADER" | python3 "$SCRIPT_DIR/planner_claude_ux.py" \
  --url "http://127.0.0.1:$PORT" --workspace "$WORKSPACE" \
  --artifacts "$ARTIFACT_DIR" --claude-bin "$claude_bin" \
  --claude-version "$claude_version" --codex-version "$codex_version" \
  --source-sha "$source_sha"
