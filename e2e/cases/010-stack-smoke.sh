#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154

CASE_NAME="stack smoke"
CASE_TIER=1
CASE_TIMEOUT_SECS=300
CASE_CHECK_SERVER_LOGS=0

stack_smoke_create_area() {
  local body area_id
  body="$(E2E_RUN_ID="$RUN_ID" \
    node -e 'process.stdout.write(JSON.stringify({name:`e2e-smoke-${process.env.E2E_RUN_ID}`,color:"#4a90d9"}))')"
  area_id="$(post_id /api/areas "$body")"
  printf '%s\n' "$area_id"
}

stack_smoke_create_track() {
  local area_id=$1 body track_id
  body="$(AREA_ID="$area_id" WORKSPACE="$WORKSPACE" \
    node -e 'process.stdout.write(JSON.stringify({area_id:process.env.AREA_ID,cwd:process.env.WORKSPACE,attach_folder:true,theme:{fg:[216,219,226],bg:[15,20,24]},title:"Tier 1 smoke track"}))')"
  track_id="$(post_id /api/tracks "$body")"
  printf '%s\n' "$track_id"
}

stack_smoke_card_ids() {
  local cards_json=$1
  printf '%s' "$cards_json" | node -e '
const fs = require("fs");
const cards = JSON.parse(fs.readFileSync(0, "utf8"));
if (!Array.isArray(cards)) {
  console.error("cards response was not an array");
  process.exit(2);
}
const planner = cards.find((c) => c.kind === "codex" && c.payload?.planner_harness === true);
const report = cards.find((c) => c.kind === "track-report");
if (typeof planner?.id !== "string") {
  console.error("planner card missing from track cards");
  process.exit(2);
}
if (typeof report?.id !== "string") {
  console.error("track-report card missing from track cards");
  process.exit(2);
}
process.stdout.write(`${planner.id}\t${report.id}\n`);
'
}

stack_smoke_frontends() {
  local origin root_headers root_status root_location
  local next_index next_deep legacy_index legacy_deep asset_path asset_meta asset_status asset_type
  origin="http://127.0.0.1:$PORT"

  root_headers="$(curl -sS -D - -o /dev/null "$origin/")" \
    || fail "curl failed for frontend root"
  root_status="$(printf '%s\n' "$root_headers" | awk 'NR == 1 { sub(/\r$/, ""); print $2 }')"
  root_location="$(printf '%s\n' "$root_headers" | awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); print $2 }')"
  [[ "$root_status" == "302" ]] \
    || fail "frontend root returned HTTP $root_status instead of 302"
  [[ "$root_location" == "/next/" ]] \
    || fail "frontend root redirected to $root_location instead of /next/"

  next_index="$(curl -fsS "$origin/next/")" \
    || fail "new frontend index was not reachable"
  next_deep="$(curl -fsS "$origin/next/track/e2e-deep-link")" \
    || fail "new frontend deep link was not reachable"
  [[ "$next_deep" == "$next_index" ]] \
    || fail "new frontend deep link did not use the SPA index fallback"

  asset_path="$(printf '%s' "$next_index" | node -e '
const fs = require("fs");
const html = fs.readFileSync(0, "utf8");
const match = html.match(/(?:src|href)="(\/next\/assets\/[^"]+\.js)"/);
if (match === null) process.exit(2);
process.stdout.write(match[1]);
')" || fail "new frontend index did not reference a /next/assets/*.js file"
  asset_meta="$(curl -sS -o /dev/null -w $'%{http_code}\t%{content_type}' "$origin$asset_path")" \
    || fail "curl failed for new frontend asset $asset_path"
  IFS=$'\t' read -r asset_status asset_type <<<"$asset_meta"
  [[ "$asset_status" == "200" ]] \
    || fail "new frontend asset $asset_path returned HTTP $asset_status"
  [[ "$asset_type" != text/html* ]] \
    || fail "new frontend asset $asset_path fell through to the SPA index"

  legacy_index="$(curl -fsS "$origin/calm/")" \
    || fail "legacy frontend compatibility index was not reachable"
  legacy_deep="$(curl -fsS "$origin/calm/track/e2e-deep-link")" \
    || fail "legacy frontend compatibility deep link was not reachable"
  [[ "$legacy_deep" == "$legacy_index" ]] \
    || fail "legacy frontend deep link did not use the SPA index fallback"
}

case_run() {
  local auth_probe_status area_id track_id cards_json planner_card_id report_card_id code

  autologin_probe
  auth_probe_status="$AUTH_PROBE_STATUS"
  stack_smoke_frontends
  init_workspace
  login_unless_autologin "$auth_probe_status"

  expect_2xx GET /api/areas -
  area_id="$(stack_smoke_create_area)"
  track_id="$(stack_smoke_create_track "$area_id")"

  expect_2xx GET "/api/tracks/$track_id/cards" -
  cards_json="$API_BODY"
  IFS=$'\t' read -r planner_card_id report_card_id \
    < <(stack_smoke_card_ids "$cards_json") \
    || fail "track $track_id did not contain planner and report cards"

  api GET "/api/cards/e2e-missing-$RUN_ID/harness/items" - \
    || fail "curl failed for bogus card GET"
  [[ "$API_STATUS" == "404" ]] \
    || fail "bogus card GET returned HTTP $API_STATUS: $(body_preview "$API_BODY")"
  code="$(json_get_string "$API_BODY" code || true)"
  [[ "$code" == "not_found" ]] \
    || fail "bogus card GET returned 404 without code=not_found: $(body_preview "$API_BODY")"

  printf 'Smoke OK area=%s track=%s planner_card=%s report_card=%s\n' \
    "$area_id" "$track_id" "$planner_card_id" "$report_card_id"
}
