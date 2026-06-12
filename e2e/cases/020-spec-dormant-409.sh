#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154

CASE_NAME="spec dormant 409 regression"
CASE_TIER=1
CASE_TIMEOUT_SECS=300
CASE_CHECK_SERVER_LOGS=0

spec_dormant_create_cove() {
  local body cove_id
  body="$(E2E_RUN_ID="$RUN_ID" \
    node -e 'process.stdout.write(JSON.stringify({name:`e2e-dormant-${process.env.E2E_RUN_ID}`,color:"#d18b47"}))')"
  cove_id="$(post_id /api/coves "$body")"
  printf '%s\n' "$cove_id"
}

spec_dormant_create_wave() {
  local cove_id=$1 body wave_id
  body="$(COVE_ID="$cove_id" WORKSPACE="$WORKSPACE" \
    node -e 'process.stdout.write(JSON.stringify({cove_id:process.env.COVE_ID,cwd:process.env.WORKSPACE,attach_folder:true,theme:{fg:[216,219,226],bg:[15,20,24]},title:"Tier 1 dormant spec regression"}))')"
  wave_id="$(post_id /api/waves "$body")"
  printf '%s\n' "$wave_id"
}

spec_dormant_card_id() {
  local cards_json=$1
  printf '%s' "$cards_json" | node -e '
const fs = require("fs");
const cards = JSON.parse(fs.readFileSync(0, "utf8"));
if (!Array.isArray(cards)) {
  console.error("cards response was not an array");
  process.exit(2);
}
const spec = cards.find((c) => c.kind === "codex" && c.payload?.spec_harness === true);
if (typeof spec?.id !== "string") {
  console.error("spec card missing from wave cards");
  process.exit(2);
}
process.stdout.write(`${spec.id}\n`);
'
}

spec_dormant_json_string_or_empty() {
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
  local auth_probe_status cove_id wave_id spec_card_id body status code runtime_id

  autologin_probe
  auth_probe_status="$AUTH_PROBE_STATUS"
  init_workspace
  login_unless_autologin "$auth_probe_status"

  cove_id="$(spec_dormant_create_cove)"
  wave_id="$(spec_dormant_create_wave "$cove_id")"

  expect_2xx GET "/api/waves/$wave_id/cards" -
  spec_card_id="$(spec_dormant_card_id "$API_BODY")" \
    || fail "wave $wave_id did not contain a spec card"

  body="$(node -e 'process.stdout.write(JSON.stringify({text:"wake dormant spec"}))')"
  api POST "/api/cards/$spec_card_id/spec/input" "$body" \
    || fail "curl failed for POST /api/cards/$spec_card_id/spec/input"
  status="$API_STATUS"
  code="$(spec_dormant_json_string_or_empty "$API_BODY" code)"

  case "$status" in
    200)
      runtime_id="$(spec_dormant_json_string_or_empty "$API_BODY" runtime_id)"
      [[ -n "$runtime_id" ]] \
        || fail "POST spec/input returned 200 without runtime_id: $(body_preview "$API_BODY")"
      skip "harness live, dormant path not reachable (runtime_id=$runtime_id)"
      ;;
    409)
      [[ "$code" == "spec_harness_dormant" ]] \
        || fail "POST spec/input returned 409 with code=$code: $(body_preview "$API_BODY")"
      printf 'Dormant OK wave=%s spec_card=%s status=%s code=%s\n' \
        "$wave_id" "$spec_card_id" "$status" "$code"
      ;;
    503)
      [[ "$code" == "service_unavailable" ]] \
        || fail "POST spec/input returned 503 with code=$code: $(body_preview "$API_BODY")"
      printf 'Dormant unavailable wave=%s spec_card=%s status=%s code=%s\n' \
        "$wave_id" "$spec_card_id" "$status" "$code"
      ;;
    404)
      fail "POST spec/input returned 404; dormant regression must not look like a missing card: $(body_preview "$API_BODY")"
      ;;
    *)
      fail "POST spec/input returned unexpected HTTP $status code=$code: $(body_preview "$API_BODY")"
      ;;
  esac
}
