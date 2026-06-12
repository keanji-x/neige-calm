#!/usr/bin/env bash
set -euo pipefail

# Opt-in local E2E. Burns real Codex tokens; do not run from CI.

: "${HOME:?HOME must be set}"

RUN_ID="e2e-$(date +%s)-$RANDOM"
DEV_ID="$RUN_ID"
PROJECT="neige-calm-$DEV_ID"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR/.." rev-parse --show-toplevel)"
ENV_FILE="$REPO_ROOT/.env"
ARTIFACT_DIR="$REPO_ROOT/e2e-artifacts"
WORKSPACE="$HOME/.cache/neige-e2e/$RUN_ID"
TMP_PREFIX="${TMPDIR:-/tmp}/neige-$RUN_ID-"
COOKIE_JAR=""; PORT=""; SERVER_CID=""

export NO_PROXY="127.0.0.1,localhost"
export no_proxy="$NO_PROXY"
fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
tmpfile() { mktemp "${TMP_PREFIX}$1.XXXXXX"; }
cleanup() {
  local status
  status=$?
  set +e
  if (( status != 0 )); then
    mkdir -p "$ARTIFACT_DIR"
    local log_file="$ARTIFACT_DIR/$RUN_ID.log"
    printf 'Failure: dumping compose logs to %s\n' "$log_file" >&2
    (cd "$REPO_ROOT" && docker compose -p "$PROJECT" logs --no-color --tail=300) >"$log_file" 2>&1 || true
  fi
  command -v docker >/dev/null 2>&1 \
    && (cd "$REPO_ROOT" && docker compose -p "$PROJECT" down -v --remove-orphans) >/dev/null 2>&1
  local workspace_prefix="$HOME/.cache/neige-e2e/"
  [[ -n "${WORKSPACE:-}" && "$WORKSPACE" == "$workspace_prefix"* ]] && rm -rf -- "$WORKSPACE"
  rm -f -- "$TMP_PREFIX"*
  trap - EXIT
  exit "$status"
}
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
  mkdir -p "$(dirname "$WORKSPACE")"
  mkdir "$WORKSPACE"
  git -C "$WORKSPACE" init -q
  git -C "$WORKSPACE" config user.name "neige e2e"
  git -C "$WORKSPACE" config user.email "neige-e2e@example.invalid"
  printf '# Neige E2E Workspace\n' >"$WORKSPACE/README.md"
  git -C "$WORKSPACE" add README.md
  git -C "$WORKSPACE" commit -q -m "initial e2e workspace"
}
start_stack() {
  printf 'Starting isolated stack: run_id=%s dev_id=%s port=%s\n' "$RUN_ID" "$DEV_ID" "$PORT"
  (cd "$REPO_ROOT" && make dev DEV_ID="$DEV_ID" COMPOSE_PROJECT_NAME="$PROJECT" CALM_PORT="$PORT")
  SERVER_CID="$(cd "$REPO_ROOT" && docker compose -p "$PROJECT" ps -q server || true)"
  [[ -n "$SERVER_CID" ]] || fail "server container was not created for compose project $PROJECT"
}
api_url() { printf 'http://127.0.0.1:%s%s' "$PORT" "$1"; }
api() {
  local method=$1 path=$2 body_file=$3 out_file=$4
  local -a args=(-sS -o "$out_file" -w '%{http_code}' -X "$method" -b "$COOKIE_JAR" -c "$COOKIE_JAR")
  [[ "$body_file" == "-" ]] || args+=(-H 'content-type: application/json' --data-binary "@$body_file")
  curl "${args[@]}" "$(api_url "$path")"
}
body_preview() { if [[ -s "$1" ]]; then tr '\n' ' ' <"$1" | cut -c1-500; fi; }
expect_2xx() {
  local method=$1 path=$2 body_file=$3 out_file=$4 status
  status="$(api "$method" "$path" "$body_file" "$out_file")" || fail "curl failed for $method $path"
  [[ "$status" == 2* ]] || fail "$method $path returned HTTP $status: $(body_preview "$out_file")"
}

json_get_string() { node -e 'const fs=require("fs");let x=JSON.parse(fs.readFileSync(process.argv[1],"utf8"));for(const p of process.argv[2].split("."))x=x?.[p];if(typeof x!=="string"||!x)process.exit(2);process.stdout.write(x);' "$1" "$2"; }

post_id() {
  local path=$1 body_file=$2 out_file id
  out_file="$(tmpfile post-out)"
  expect_2xx POST "$path" "$body_file" "$out_file"
  id="$(json_get_string "$out_file" id)" || fail "$path response did not contain id"
  rm -f -- "$out_file"
  printf '%s\n' "$id"
}

check_server_logs() {
  [[ -n "${SERVER_CID:-}" ]] || SERVER_CID="$(cd "$REPO_ROOT" && docker compose -p "$PROJECT" ps -q server || true)"
  [[ -n "$SERVER_CID" ]] || return 0
  local match
  match="$(docker logs --tail 500 "$SERVER_CID" 2>&1 \
    | grep -F -e 'spec harness start submission failed' -e 'wave created but spec agent is inert' \
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
  COOKIE_JAR="$(tmpfile cookie)"
  local body_file out_file auth_user auth_password
  body_file="$(tmpfile login-body)"
  out_file="$(tmpfile login-out)"
  auth_user="$(dotenv_get CALM_AUTH_USERNAME || printf 'owner')"
  auth_password="$(dotenv_get CALM_AUTH_PASSWORD || printf 'dev')"
  AUTH_USER="$auth_user" AUTH_PASSWORD="$auth_password" \
    node -e 'process.stdout.write(JSON.stringify({username:process.env.AUTH_USER,password:process.env.AUTH_PASSWORD}))' >"$body_file"
  expect_2xx POST /api/auth/login "$body_file" "$out_file"
  rm -f -- "$body_file" "$out_file"
}

create_cove() {
  local body_file cove_id
  body_file="$(tmpfile cove-body)"
  E2E_RUN_ID="$RUN_ID" \
    node -e 'process.stdout.write(JSON.stringify({name:`e2e-${process.env.E2E_RUN_ID}`,color:"#4a90d9"}))' >"$body_file"
  cove_id="$(post_id /api/coves "$body_file")"
  rm -f -- "$body_file"
  printf '%s\n' "$cove_id"
}

create_wave() {
  local cove_id=$1 body_file wave_id
  body_file="$(tmpfile wave-body)"
  COVE_ID="$cove_id" WORKSPACE="$WORKSPACE" node -e 'process.stdout.write(JSON.stringify({cove_id:process.env.COVE_ID,cwd:process.env.WORKSPACE,attach_folder:true,theme:{fg:[220,220,220],bg:[30,30,30]},title:"Dispatch exactly 2 parallel codex workers: create src/greet.py greet(name) returning '\''Hello, {name}!'\''; create USAGE.md docs; then summarize report."}))' >"$body_file"
  wave_id="$(post_id /api/waves "$body_file")"
  rm -f -- "$body_file"
  printf '%s\n' "$wave_id"
}

summarize_state() {
  node - "$1" "$2" "$WORKSPACE" <<'NODE'
const fs = require("fs"), p = require("path");
const [cf, df, ws] = process.argv.slice(2);
const cards = JSON.parse(fs.readFileSync(cf, "utf8"));
const wave = JSON.parse(fs.readFileSync(df, "utf8")).wave ?? {};
const workers = cards.filter((c) => c.kind === "codex" && !(c.payload && c.payload.spec_harness === true));
const report = cards.find((c) => c.kind === "wave-report");
const body = typeof report?.payload?.body === "string" ? report.payload.body : "";
const flags = [
  fs.existsSync(p.join(ws, "src", "greet.py")) ? "yes" : "no",
  fs.existsSync(p.join(ws, "USAGE.md")) ? "yes" : "no",
  body && body !== "# Goal\n\n_The spec agent will fill this in._\n" ? "changed" : "placeholder",
  workers.length >= 2 && workers.every((c) => c.runtime && c.runtime.status !== "running") ? "yes" : "no",
  wave.lifecycle === "reviewing" || wave.lifecycle === "done" ? "yes" : "no",
];
process.stdout.write([wave.lifecycle ?? "unknown", workers.length, workers.map((c) => c.runtime?.status ?? "none").join(",") || "-", ...flags].join("\t"));
NODE
}

poll_wave() {
  local wave_id=$1 start=$SECONDS total=1200 stage1=300 stage1_ok=0
  while (( SECONDS - start <= total )); do
    check_server_logs
    local cards_file detail_file elapsed lifecycle worker_count worker_statuses greet usage report workers_done lifecycle_ready
    cards_file="$(tmpfile cards)"
    detail_file="$(tmpfile detail)"
    expect_2xx GET "/api/waves/$wave_id/cards" - "$cards_file"
    expect_2xx GET "/api/waves/$wave_id" - "$detail_file"
    IFS=$'\t' read -r lifecycle worker_count worker_statuses greet usage report workers_done lifecycle_ready \
      < <(summarize_state "$cards_file" "$detail_file")
    rm -f -- "$cards_file" "$detail_file"

    elapsed=$((SECONDS - start))
    printf 'poll +%04ds lifecycle=%s workers=%s statuses=%s files=greet:%s usage:%s report:%s\n' \
      "$elapsed" "$lifecycle" "$worker_count" "$worker_statuses" "$greet" "$usage" "$report"

    if (( worker_count >= 2 )); then
      stage1_ok=1
    elif (( elapsed >= stage1 )); then
      fail "stage 1 timed out: fewer than 2 non-spec codex worker cards after 5 minutes"
    fi

    if (( stage1_ok == 1 )) \
      && [[ "$workers_done" == "yes" && "$greet" == "yes" && "$usage" == "yes" ]] \
      && [[ "$report" == "changed" && "$lifecycle_ready" == "yes" ]]; then
      printf 'PASS run_id=%s wave_id=%s workers=%s lifecycle=%s workspace=%s\n' \
        "$RUN_ID" "$wave_id" "$worker_count" "$lifecycle" "$WORKSPACE"
      return 0
    fi

    (( elapsed < total )) || break
    sleep 10
  done
  fail "stage 2 timed out after 20 minutes before workers, files, report, and lifecycle were complete"
}

main() {
  cd "$REPO_ROOT" || exit 1
  preflight
  PORT="$(pick_port)"
  init_workspace; start_stack; wait_for_health; login
  local cove_id wave_id
  cove_id="$(create_cove)"
  wave_id="$(create_wave "$cove_id")"
  printf 'Created cove: %s\nCreated wave: %s\n' "$cove_id" "$wave_id"
  poll_wave "$wave_id"
}

main "$@"
