#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154

api_url() {
  printf 'http://127.0.0.1:%s%s' "$PORT" "$1"
}

dotenv_get() {
  local key=$1
  local line value

  if [[ -n "${!key:-}" ]]; then
    printf '%s\n' "${!key}"
    return 0
  fi

  [[ -f "$ENV_FILE" ]] || return 1
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

api() {
  local method=$1 path=$2 body=$3 response
  local -a args=(-sS -o - -w $'\n__NEIGE_HTTP_STATUS__:%{http_code}' -X "$method")
  [[ -z "$COOKIE_HEADER" ]] || args+=(-H "Cookie: $COOKIE_HEADER")
  [[ "$body" == "-" ]] || args+=(-H 'content-type: application/json' --data-binary "$body")
  response="$(curl "${args[@]}" "$(api_url "$path")")" || return 1
  API_STATUS="${response##*__NEIGE_HTTP_STATUS__:}"
  API_BODY="${response%$'\n'__NEIGE_HTTP_STATUS__:*}"
}

body_preview() {
  printf '%s' "$1" | tr '\n' ' ' | cut -c1-500
}

expect_2xx() {
  local method=$1 path=$2 body=$3
  api "$method" "$path" "$body" || fail "curl failed for $method $path"
  [[ "$API_STATUS" == 2* ]] || fail "$method $path returned HTTP $API_STATUS: $(body_preview "$API_BODY")"
}

json_get_string() {
  printf '%s' "$1" | node -e 'const fs=require("fs");let x=JSON.parse(fs.readFileSync(0,"utf8"));for(const p of process.argv[1].split("."))x=x?.[p];if(typeof x!=="string"||!x)process.exit(2);process.stdout.write(x);' "$2"
}

post_id() {
  local path=$1 body=$2 id
  expect_2xx POST "$path" "$body"
  id="$(json_get_string "$API_BODY" id)" || fail "$path response did not contain id"
  printf '%s\n' "$id"
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

autologin_probe() {
  api GET /api/coves - || fail "curl failed for GET /api/coves auth probe"
  AUTH_PROBE_STATUS="$API_STATUS"
  [[ "$AUTH_PROBE_STATUS" == 2* || "$AUTH_PROBE_STATUS" == "401" ]] \
    || fail "GET /api/coves auth probe returned HTTP $AUTH_PROBE_STATUS: $(body_preview "$API_BODY")"
}

login_unless_autologin() {
  local auth_probe_status=$1
  if [[ "$auth_probe_status" == 2* ]]; then
    printf 'Auth probe: cookie-less requests accepted; skipping login\n'
  else
    login
  fi
}
