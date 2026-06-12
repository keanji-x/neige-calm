#!/usr/bin/env bash
set -euo pipefail
set -E

# Opt-in local E2E. Burns real Codex tokens; do not run from CI.
# Host writes are not zero: make dev writes standard build outputs (target/,
# web/dist) and XDG dirs, rebuilds the shared neige-calm-server:local image tag,
# and may create/reuse the shared proxy-forwarder container. The no-host-writes
# expectation applies only to test data/workspace; e2e-artifacts/ is written when
# a run fails.

: "${HOME:?HOME must be set}"

RUN_ID="e2e-$(date +%s)-$RANDOM"
DEV_ID="$RUN_ID"
PROJECT="neige-calm-$DEV_ID"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR/.." rev-parse --show-toplevel)"
ENV_FILE="$REPO_ROOT/.env"
ARTIFACT_DIR="$REPO_ROOT/e2e-artifacts"
WORKSPACE=""
SERVER_CONTAINER="neige-calm-$DEV_ID-server-1"
COOKIE_HEADER=""; PORT=""; SERVER_CID=""

export NO_PROXY="127.0.0.1,localhost"
export no_proxy="$NO_PROXY"
fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
cleanup() {
  local status=$?
  trap - EXIT ERR
  set +e
  printf 'Exit status: %s\n' "$status" >&2
  if (( status != 0 )); then
    mkdir -p "$ARTIFACT_DIR"
    local log_file="$ARTIFACT_DIR/$RUN_ID.log"
    printf 'Failure: dumping compose logs to %s\n' "$log_file" >&2
    (cd "$REPO_ROOT" && docker compose -p "$PROJECT" logs --no-color --tail=300) >"$log_file" 2>&1 || true
  fi
  if command -v docker >/dev/null 2>&1; then
    if ! (cd "$REPO_ROOT" && docker compose -p "$PROJECT" down -v --remove-orphans) >/dev/null 2>&1; then
      printf 'WARN: docker compose down failed for project %s\n' "$PROJECT" >&2
    fi
  fi
  exit "$status"
}
on_err() {
  local status=$1 line=$2 command=$3
  printf 'ERR: line %s: %s\n' "$line" "$command" >&2
  return "$status"
}
trap 'on_err "$?" "$LINENO" "$BASH_COMMAND"' ERR
trap cleanup EXIT
dotenv_get() {
  local key=$1 line value
  line="$(grep -E "^${key}=" "$ENV_FILE" | tail -n 1 || true)"
  [[ -n "$line" ]] || return 1
  value="${line#*=}"
  value="${value%$'\r'}"
  if [[ "$value" == \"*\" && "$value" == *\" ]]; then
    value="${value:1:${#value}-2}"
  elif [[ "$value" == \'*\' && "$value" == *\' ]]; then
    value="${value:1:${#value}-2}"
  fi
  printf '%s\n' "$value"
}
preflight() {
  local bin node_major
  for bin in docker make git curl ss cargo; do
    command -v "$bin" >/dev/null 2>&1 || fail "$bin is required on PATH"
  done
  docker info >/dev/null 2>&1 || fail "docker daemon is not reachable"
  docker compose version >/dev/null 2>&1 || fail "docker compose plugin is required"
  [[ -f "$ENV_FILE" ]] || fail "missing $ENV_FILE; create/copy .env for this checkout"
  [[ -s "$HOME/.codex/auth.json" ]] || fail "missing Codex auth at $HOME/.codex/auth.json; run codex login"

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
init_workspace() {
  docker exec "$SERVER_CONTAINER" sh -lc '
    set -e
    ws=$1
    mkdir -p "$ws"
    git -C "$ws" init -q
    git -C "$ws" -c user.email=e2e@test -c user.name=e2e commit --allow-empty -q -m "initial e2e workspace"
  ' sh "$WORKSPACE"
}
start_stack() {
  printf 'Starting isolated stack: run_id=%s dev_id=%s port=%s\n' "$RUN_ID" "$DEV_ID" "$PORT"
  (cd "$REPO_ROOT" && make dev DEV_ID="$DEV_ID" COMPOSE_PROJECT_NAME="$PROJECT" CALM_PORT="$PORT")
  SERVER_CID="$(cd "$REPO_ROOT" && docker compose -p "$PROJECT" ps -q server || true)"
  [[ -n "$SERVER_CID" ]] || fail "server container was not created for compose project $PROJECT"
}
api_url() { printf 'http://127.0.0.1:%s%s' "$PORT" "$1"; }
api() {
  local method=$1 path=$2 body=$3 response
  local -a args=(-sS -o - -w $'\n__NEIGE_HTTP_STATUS__:%{http_code}' -X "$method")
  [[ -z "$COOKIE_HEADER" ]] || args+=(-H "Cookie: $COOKIE_HEADER")
  [[ "$body" == "-" ]] || args+=(-H 'content-type: application/json' --data-binary "$body")
  response="$(curl "${args[@]}" "$(api_url "$path")")" || return 1
  API_STATUS="${response##*__NEIGE_HTTP_STATUS__:}"
  API_BODY="${response%$'\n'__NEIGE_HTTP_STATUS__:*}"
}
body_preview() { printf '%s' "$1" | tr '\n' ' ' | cut -c1-500; }
expect_2xx() {
  local method=$1 path=$2 body=$3
  api "$method" "$path" "$body" || fail "curl failed for $method $path"
  [[ "$API_STATUS" == 2* ]] || fail "$method $path returned HTTP $API_STATUS: $(body_preview "$API_BODY")"
}

json_get_string() { printf '%s' "$1" | node -e 'const fs=require("fs");let x=JSON.parse(fs.readFileSync(0,"utf8"));for(const p of process.argv[1].split("."))x=x?.[p];if(typeof x!=="string"||!x)process.exit(2);process.stdout.write(x);' "$2"; }

post_id() {
  local path=$1 body=$2 id
  expect_2xx POST "$path" "$body"
  id="$(json_get_string "$API_BODY" id)" || fail "$path response did not contain id"
  printf '%s\n' "$id"
}

check_server_logs() {
  [[ -n "${SERVER_CID:-}" ]] || SERVER_CID="$(cd "$REPO_ROOT" && docker compose -p "$PROJECT" ps -q server || true)"
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
    status="$(curl -sS -o /dev/null -w '%{http_code}' "$(api_url /api/coves)" 2>/dev/null || true)"
    if [[ "$status" == 2* || "$status" == "401" ]]; then
      printf 'Health ready: HTTP %s\n' "$status"
      return 0
    fi
    sleep 2
  done
  fail "health check timed out after 120s waiting for $(api_url /api/coves)"
}

login() {
  local body response headers status auth_user auth_password
  auth_user="$(dotenv_get CALM_AUTH_USERNAME || printf 'owner')"
  auth_password="$(dotenv_get CALM_AUTH_PASSWORD || printf 'dev')"
  body="$(AUTH_USER="$auth_user" AUTH_PASSWORD="$auth_password" \
    node -e 'process.stdout.write(JSON.stringify({username:process.env.AUTH_USER,password:process.env.AUTH_PASSWORD}))')"
  response="$(curl -sS -D - -o /dev/null -w $'\n__NEIGE_HTTP_STATUS__:%{http_code}' \
    -X POST -H 'content-type: application/json' --data-binary "$body" "$(api_url /api/auth/login)")" \
    || fail "curl failed for POST /api/auth/login"
  status="${response##*__NEIGE_HTTP_STATUS__:}"
  headers="${response%$'\n'__NEIGE_HTTP_STATUS__:*}"
  [[ "$status" == 2* ]] || fail "POST /api/auth/login returned HTTP $status"
  COOKIE_HEADER="$(printf '%s\n' "$headers" | awk 'BEGIN{first=1} /^[Ss]et-[Cc]ookie:[[:space:]]*/ {sub(/\r$/,""); sub(/^[^:]*:[[:space:]]*/,""); split($0,a,";"); if(!first) printf "; "; printf "%s", a[1]; first=0}')"
  [[ -n "$COOKIE_HEADER" ]] || fail "POST /api/auth/login did not set a cookie"
}

create_cove() {
  local body cove_id
  body="$(E2E_RUN_ID="$RUN_ID" \
    node -e 'process.stdout.write(JSON.stringify({name:`e2e-${process.env.E2E_RUN_ID}`,color:"#4a90d9"}))')"
  cove_id="$(post_id /api/coves "$body")"
  printf '%s\n' "$cove_id"
}

create_wave() {
  local cove_id=$1 body wave_id
  body="$(COVE_ID="$cove_id" WORKSPACE="$WORKSPACE" node -e 'process.stdout.write(JSON.stringify({cove_id:process.env.COVE_ID,cwd:process.env.WORKSPACE,attach_folder:true,theme:{fg:[220,220,220],bg:[30,30,30]},title:"Dispatch exactly 2 parallel codex workers: create src/greet.py greet(name) returning '\''Hello, {name}!'\''; create USAGE.md docs; then summarize report."}))')"
  wave_id="$(post_id /api/waves "$body")"
  printf '%s\n' "$wave_id"
}

summarize_state() {
  local cards_json=$1 detail_json=$2 greet=$3 usage=$4
  printf '%s\0%s' "$cards_json" "$detail_json" | GREET="$greet" USAGE="$usage" node -e '
const fs = require("fs");
const raw = fs.readFileSync(0);
const sep = raw.indexOf(0);
if (sep < 0) process.exit(2);
const cards = JSON.parse(raw.subarray(0, sep).toString("utf8"));
const wave = JSON.parse(raw.subarray(sep + 1).toString("utf8")).wave ?? {};
const workers = cards.filter((c) => c.kind === "codex" && !(c.payload && c.payload.spec_harness === true));
const report = cards.find((c) => c.kind === "wave-report");
const body = typeof report?.payload?.body === "string" ? report.payload.body : "";
const flags = [
  process.env.GREET,
  process.env.USAGE,
  body.length > 0 && !body.includes("_The spec agent will fill this in._") ? "changed" : "placeholder",
  wave.lifecycle === "done" ? "yes" : "no",
];
process.stdout.write([wave.lifecycle ?? "unknown", workers.length, workers.map((c) => c.runtime?.status ?? "none").join(",") || "-", ...flags].join("\t") + "\n");
'
}

poll_wave() {
  local wave_id=$1 start=$SECONDS total=1200 stage1=300 stage1_ok=0
  while (( SECONDS - start <= total )); do
    check_server_logs
    local cards_json detail_json elapsed lifecycle worker_count worker_statuses greet usage report lifecycle_ready
    expect_2xx GET "/api/waves/$wave_id/cards" -
    cards_json="$API_BODY"
    expect_2xx GET "/api/waves/$wave_id" -
    detail_json="$API_BODY"
    if docker exec "$SERVER_CONTAINER" test -f "$WORKSPACE/src/greet.py"; then greet=yes; else greet=no; fi
    if docker exec "$SERVER_CONTAINER" test -f "$WORKSPACE/USAGE.md"; then usage=yes; else usage=no; fi
    IFS=$'\t' read -r lifecycle worker_count worker_statuses greet usage report lifecycle_ready \
      < <(summarize_state "$cards_json" "$detail_json" "$greet" "$usage") \
      || fail 'summarize_state produced no parsable state'

    elapsed=$((SECONDS - start))
    printf 'poll +%04ds lifecycle=%s workers=%s statuses=%s files=greet:%s usage:%s report:%s\n' \
      "$elapsed" "$lifecycle" "$worker_count" "$worker_statuses" "$greet" "$usage" "$report"

    if (( worker_count >= 2 )); then
      stage1_ok=1
    elif (( elapsed >= stage1 )); then
      fail "stage 1 timed out: fewer than 2 non-spec codex worker cards after 5 minutes"
    fi

    if (( stage1_ok == 1 )) \
      && [[ "$greet" == "yes" && "$usage" == "yes" ]] \
      && [[ "$report" == "changed" && "$lifecycle_ready" == "yes" ]]; then
      printf 'PASS run_id=%s wave_id=%s workers=%s lifecycle=%s workspace=%s\n' \
        "$RUN_ID" "$wave_id" "$worker_count" "$lifecycle" "$WORKSPACE"
      return 0
    fi

    (( elapsed < total )) || break
    sleep 10
  done
  fail "stage 2 timed out after 20 minutes before files, report, and lifecycle were complete"
}

main() {
  local auth_probe_status
  cd "$REPO_ROOT" || exit 1
  preflight
  WORKSPACE="$(dotenv_get CALM_CONTAINER_STATE_DIR || printf '/var/lib/neige-calm')/e2e-workspace"
  PORT="$(pick_port)"
  start_stack; wait_for_health
  api GET /api/coves - || fail "curl failed for GET /api/coves auth probe"
  auth_probe_status="$API_STATUS"
  [[ "$auth_probe_status" == 2* || "$auth_probe_status" == "401" ]] \
    || fail "GET /api/coves auth probe returned HTTP $auth_probe_status: $(body_preview "$API_BODY")"
  init_workspace
  if [[ "$auth_probe_status" == 2* ]]; then
    printf 'Auth probe: cookie-less requests accepted; skipping login\n'
  else
    login
  fi
  local cove_id wave_id
  cove_id="$(create_cove)"
  wave_id="$(create_wave "$cove_id")"
  printf 'Created cove: %s\nCreated wave: %s\n' "$cove_id" "$wave_id"
  poll_wave "$wave_id"
}

main "$@"
