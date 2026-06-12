#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154

: "${HOME:?HOME must be set}"

E2E_CONTAINER_STATE_DIR="${E2E_CONTAINER_STATE_DIR:-/var/lib/neige-calm}"

stack_preflight() {
  local need_codex_auth=${1:-0}
  local bin node_major

  for bin in docker make git curl ss cargo; do
    command -v "$bin" >/dev/null 2>&1 || fail "$bin is required on PATH"
  done
  docker info >/dev/null 2>&1 || fail "docker daemon is not reachable"
  docker compose version >/dev/null 2>&1 || fail "docker compose plugin is required"
  [[ -f "$ENV_FILE" ]] || fail "missing $ENV_FILE; create/copy .env for this checkout"
  if (( need_codex_auth )); then
    [[ -s "$HOME/.codex/auth.json" ]] || fail "missing Codex auth at $HOME/.codex/auth.json; run codex login"
  fi

  if [[ -s "$HOME/.nvm/nvm.sh" ]]; then
    # shellcheck source=/dev/null
    set +u
    source "$HOME/.nvm/nvm.sh"
    nvm use --silent node >/dev/null 2>&1 || true
    set -u
  fi
  command -v node >/dev/null 2>&1 || fail "node >=20 is required on PATH; activate it before running"
  node_major="$(node -p 'Number(process.versions.node.split(".")[0])')" || fail "unable to read node version"
  (( node_major >= 20 )) || fail "node >=20 is required; found $(node -v). Activate Node 20+ before running"
}

pick_port() {
  local port=4900
  while ss -H -ltn "sport = :$port" 2>/dev/null | grep -q .; do
    port=$((port + 1))
    (( port < 65000 )) || fail "no free TCP port found starting at 4900"
  done
  printf '%s\n' "$port"
}

resolve_server_cid() {
  SERVER_CID="$(cd "$REPO_ROOT" && docker compose -p "$PROJECT" ps -q server || true)"
}

init_workspace() {
  docker exec "$SERVER_CID" sh -lc '
    set -e
    ws=$1
    mkdir -p "$ws"
    git -C "$ws" init -q
    git -C "$ws" -c user.email=e2e@test -c user.name=e2e commit --allow-empty -q -m "initial e2e workspace"
  ' sh "$WORKSPACE"
}

start_stack() {
  local -a state_neutralizers=(
    # Empty is deliberate: compose uses ${VAR:-default} for these paths, and
    # Makefile gates RESET_DB/FRESH on literal "1".
    CALM_CONTAINER_STATE_DIR=
    CALM_DB_URL=
    CALM_DATA_DIR=
    CALM_PLUGINS_DATA_DIR=
    RESET_DB=
    FRESH=
  )
  printf 'Starting isolated stack: run_id=%s dev_id=%s port=%s\n' "$RUN_ID" "$DEV_ID" "$PORT"
  (cd "$REPO_ROOT" && make dev DEV_ID="$DEV_ID" COMPOSE_PROJECT_NAME="$PROJECT" CALM_PORT="$PORT" "${state_neutralizers[@]}")
  resolve_server_cid
  [[ -n "$SERVER_CID" ]] || fail "server container was not created for compose project $PROJECT"
}

check_server_logs() {
  [[ -n "${SERVER_CID:-}" ]] || resolve_server_cid
  [[ -n "$SERVER_CID" ]] || return 0
  local match
  match="$(docker logs --tail 500 "$SERVER_CID" 2>&1 \
    | grep -F -e 'spec harness start submission failed' -e 'spec harness start wait failed' -e 'wave created but spec agent is inert' \
    | tail -n 1 || true)"
  [[ -z "$match" ]] || fail "server logs contain fatal spec-harness warning: $match"
}

wait_for_health() {
  local deadline=$((SECONDS + 120)) status
  while (( SECONDS < deadline )); do
    check_server_logs
    status="$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/api/coves" 2>/dev/null || true)"
    if [[ "$status" == 2* || "$status" == "401" ]]; then
      printf 'Health ready: HTTP %s\n' "$status"
      return 0
    fi
    sleep 2
  done
  fail "health check timed out after 120s waiting for http://127.0.0.1:$PORT/api/coves"
}

dump_artifacts() {
  mkdir -p "$ARTIFACT_DIR"
  local log_file="$ARTIFACT_DIR/$RUN_ID.log"
  printf 'Failure: dumping compose logs to %s\n' "$log_file" >&2
  (cd "$REPO_ROOT" && docker compose -p "$PROJECT" logs --no-color --tail=300) >"$log_file" 2>&1 || true
}

teardown_stack() {
  if command -v docker >/dev/null 2>&1; then
    if ! (cd "$REPO_ROOT" && docker compose -p "$PROJECT" down -v --remove-orphans) >/dev/null 2>&1; then
      printf 'WARN: docker compose down failed for project %s\n' "$PROJECT" >&2
    fi
  fi
}

e2e_cleanup() {
  local status=$?
  trap - EXIT ERR
  set +e
  printf 'Exit status: %s\n' "$status" >&2
  if (( status != 0 )); then
    dump_artifacts
  fi
  teardown_stack
  exit "$status"
}
