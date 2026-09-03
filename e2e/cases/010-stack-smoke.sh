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
const spec = cards.find((c) => c.kind === "codex" && c.payload?.spec_harness === true);
const report = cards.find((c) => c.kind === "track-report");
if (typeof spec?.id !== "string") {
  console.error("spec card missing from track cards");
  process.exit(2);
}
if (typeof report?.id !== "string") {
  console.error("track-report card missing from track cards");
  process.exit(2);
}
process.stdout.write(`${spec.id}\t${report.id}\n`);
'
}

case_run() {
  local auth_probe_status area_id track_id cards_json spec_card_id report_card_id code

  autologin_probe
  auth_probe_status="$AUTH_PROBE_STATUS"
  init_workspace
  login_unless_autologin "$auth_probe_status"

  expect_2xx GET /api/areas -
  area_id="$(stack_smoke_create_area)"
  track_id="$(stack_smoke_create_track "$area_id")"

  expect_2xx GET "/api/tracks/$track_id/cards" -
  cards_json="$API_BODY"
  IFS=$'\t' read -r spec_card_id report_card_id \
    < <(stack_smoke_card_ids "$cards_json") \
    || fail "track $track_id did not contain spec and report cards"

  api GET "/api/cards/e2e-missing-$RUN_ID/harness/items" - \
    || fail "curl failed for bogus card GET"
  [[ "$API_STATUS" == "404" ]] \
    || fail "bogus card GET returned HTTP $API_STATUS: $(body_preview "$API_BODY")"
  code="$(json_get_string "$API_BODY" code || true)"
  [[ "$code" == "not_found" ]] \
    || fail "bogus card GET returned 404 without code=not_found: $(body_preview "$API_BODY")"

  printf 'Smoke OK area=%s track=%s spec_card=%s report_card=%s\n' \
    "$area_id" "$track_id" "$spec_card_id" "$report_card_id"
}
