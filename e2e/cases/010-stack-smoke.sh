#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154

CASE_NAME="stack smoke"
CASE_TIER=1
CASE_TIMEOUT_SECS=300
CASE_CHECK_SERVER_LOGS=0

stack_smoke_create_cove() {
  local body cove_id
  body="$(E2E_RUN_ID="$RUN_ID" \
    node -e 'process.stdout.write(JSON.stringify({name:`e2e-smoke-${process.env.E2E_RUN_ID}`,color:"#4a90d9"}))')"
  cove_id="$(post_id /api/coves "$body")"
  printf '%s\n' "$cove_id"
}

stack_smoke_create_wave() {
  local cove_id=$1 body wave_id
  body="$(COVE_ID="$cove_id" WORKSPACE="$WORKSPACE" \
    node -e 'process.stdout.write(JSON.stringify({cove_id:process.env.COVE_ID,cwd:process.env.WORKSPACE,attach_folder:true,theme:{fg:[216,219,226],bg:[15,20,24]},title:"Tier 1 smoke wave"}))')"
  wave_id="$(post_id /api/waves "$body")"
  printf '%s\n' "$wave_id"
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
const report = cards.find((c) => c.kind === "wave-report");
if (typeof spec?.id !== "string") {
  console.error("spec card missing from wave cards");
  process.exit(2);
}
if (typeof report?.id !== "string") {
  console.error("wave-report card missing from wave cards");
  process.exit(2);
}
process.stdout.write(`${spec.id}\t${report.id}\n`);
'
}

case_run() {
  local auth_probe_status cove_id wave_id cards_json spec_card_id report_card_id code

  autologin_probe
  auth_probe_status="$AUTH_PROBE_STATUS"
  init_workspace
  login_unless_autologin "$auth_probe_status"

  expect_2xx GET /api/coves -
  cove_id="$(stack_smoke_create_cove)"
  wave_id="$(stack_smoke_create_wave "$cove_id")"

  expect_2xx GET "/api/waves/$wave_id/cards" -
  cards_json="$API_BODY"
  IFS=$'\t' read -r spec_card_id report_card_id \
    < <(stack_smoke_card_ids "$cards_json") \
    || fail "wave $wave_id did not contain spec and report cards"

  api GET "/api/cards/e2e-missing-$RUN_ID/harness/items" - \
    || fail "curl failed for bogus card GET"
  [[ "$API_STATUS" == "404" ]] \
    || fail "bogus card GET returned HTTP $API_STATUS: $(body_preview "$API_BODY")"
  code="$(json_get_string "$API_BODY" code || true)"
  [[ "$code" == "not_found" ]] \
    || fail "bogus card GET returned 404 without code=not_found: $(body_preview "$API_BODY")"

  printf 'Smoke OK cove=%s wave=%s spec_card=%s report_card=%s\n' \
    "$cove_id" "$wave_id" "$spec_card_id" "$report_card_id"
}
