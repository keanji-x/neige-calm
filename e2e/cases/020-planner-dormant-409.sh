#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154

CASE_NAME="planner dormant 409 regression"
CASE_TIER=1
CASE_TIMEOUT_SECS=300
CASE_CHECK_SERVER_LOGS=0

planner_dormant_create_area() {
  local body area_id
  body="$(E2E_RUN_ID="$RUN_ID" \
    node -e 'process.stdout.write(JSON.stringify({name:`e2e-dormant-${process.env.E2E_RUN_ID}`,color:"#d18b47"}))')"
  area_id="$(post_id /api/areas "$body")"
  printf '%s\n' "$area_id"
}

planner_dormant_create_track() {
  local area_id=$1 body track_id
  body="$(AREA_ID="$area_id" WORKSPACE="$WORKSPACE" \
    node -e 'process.stdout.write(JSON.stringify({area_id:process.env.AREA_ID,cwd:process.env.WORKSPACE,attach_folder:true,theme:{fg:[216,219,226],bg:[15,20,24]},title:"Tier 1 dormant planner regression"}))')"
  track_id="$(post_id /api/tracks "$body")"
  printf '%s\n' "$track_id"
}

planner_dormant_card_id() {
  local cards_json=$1
  printf '%s' "$cards_json" | node -e '
const fs = require("fs");
const cards = JSON.parse(fs.readFileSync(0, "utf8"));
if (!Array.isArray(cards)) {
  console.error("cards response was not an array");
  process.exit(2);
}
const planner = cards.find((c) => c.kind === "codex" && c.payload?.planner_harness === true);
if (typeof planner?.id !== "string") {
  console.error("planner card missing from track cards");
  process.exit(2);
}
process.stdout.write(`${planner.id}\n`);
'
}

planner_dormant_json_string_or_empty() {
  local json=$1 path=$2
  printf '%s' "$json" | node -e '
const fs = require("fs");
let value;
try {
  value = JSON.parse(fs.readFileSync(0, "utf8"));
} catch {
  process.exit(0);
}
for (const part of process.argv[1].split(".")) value = value?.[part];
if (typeof value === "string" && value.length > 0) process.stdout.write(value);
' "$path"
}

case_run() {
  local auth_probe_status area_id track_id planner_card_id body status code runtime_id

  autologin_probe
  auth_probe_status="$AUTH_PROBE_STATUS"
  init_workspace
  login_unless_autologin "$auth_probe_status"

  area_id="$(planner_dormant_create_area)"
  track_id="$(planner_dormant_create_track "$area_id")"

  expect_2xx GET "/api/tracks/$track_id/cards" -
  planner_card_id="$(planner_dormant_card_id "$API_BODY")" \
    || fail "track $track_id did not contain a planner card"

  body="$(node -e 'process.stdout.write(JSON.stringify({text:"wake dormant planner"}))')"
  api POST "/api/cards/$planner_card_id/planner/input" "$body" \
    || fail "curl failed for POST /api/cards/$planner_card_id/planner/input"
  status="$API_STATUS"
  code="$(planner_dormant_json_string_or_empty "$API_BODY" code)"

  case "$status" in
    200)
      runtime_id="$(planner_dormant_json_string_or_empty "$API_BODY" runtime_id)"
      [[ -n "$runtime_id" ]] \
        || fail "POST planner/input returned 200 without runtime_id: $(body_preview "$API_BODY")"
      skip "harness live, dormant path not reachable (runtime_id=$runtime_id)"
      ;;
    409)
      [[ "$code" == "planner_harness_dormant" ]] \
        || fail "POST planner/input returned 409 with code=$code: $(body_preview "$API_BODY")"
      printf 'Dormant OK track=%s planner_card=%s status=%s code=%s\n' \
        "$track_id" "$planner_card_id" "$status" "$code"
      ;;
    503)
      [[ "$code" == "service_unavailable" ]] \
        || fail "POST planner/input returned 503 with code=$code: $(body_preview "$API_BODY")"
      skip "daemon down (503), dormant 409 path not reached"
      ;;
    404)
      fail "POST planner/input returned 404; dormant regression must not look like a missing card: $(body_preview "$API_BODY")"
      ;;
    *)
      fail "POST planner/input returned unexpected HTTP $status code=$code: $(body_preview "$API_BODY")"
      ;;
  esac
}
