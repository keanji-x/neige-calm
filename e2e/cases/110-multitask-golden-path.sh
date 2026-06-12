#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154

CASE_NAME="multitask golden path"
CASE_TIER=2
CASE_TIMEOUT_SECS=1500

multitask_create_cove() {
  local body cove_id
  body="$(E2E_RUN_ID="$RUN_ID" \
    node -e 'process.stdout.write(JSON.stringify({name:`e2e-${process.env.E2E_RUN_ID}`,color:"#4a90d9"}))')"
  cove_id="$(post_id /api/coves "$body")"
  printf '%s\n' "$cove_id"
}

multitask_create_wave() {
  local cove_id=$1 body wave_id
  body="$(COVE_ID="$cove_id" WORKSPACE="$WORKSPACE" node -e 'process.stdout.write(JSON.stringify({cove_id:process.env.COVE_ID,cwd:process.env.WORKSPACE,attach_folder:true,theme:{fg:[220,220,220],bg:[30,30,30]},title:"Dispatch exactly 2 parallel codex workers: create src/greet.py greet(name) returning '\''Hello, {name}!'\''; create USAGE.md docs; then summarize report."}))')"
  wave_id="$(post_id /api/waves "$body")"
  printf '%s\n' "$wave_id"
}

multitask_summarize_state() {
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

multitask_poll_wave() {
  local wave_id=$1 start=$SECONDS total=1200 stage1=300 stage1_ok=0
  while (( SECONDS - start <= total )); do
    check_server_logs
    local cards_json detail_json elapsed lifecycle worker_count worker_statuses greet usage report lifecycle_ready
    expect_2xx GET "/api/waves/$wave_id/cards" -
    cards_json="$API_BODY"
    expect_2xx GET "/api/waves/$wave_id" -
    detail_json="$API_BODY"
    if file_in_container "$WORKSPACE/src/greet.py"; then greet=yes; else greet=no; fi
    if file_in_container "$WORKSPACE/USAGE.md"; then usage=yes; else usage=no; fi
    IFS=$'\t' read -r lifecycle worker_count worker_statuses greet usage report lifecycle_ready \
      < <(multitask_summarize_state "$cards_json" "$detail_json" "$greet" "$usage") \
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

case_run() {
  local auth_probe_status cove_id wave_id

  autologin_probe
  auth_probe_status="$AUTH_PROBE_STATUS"
  init_workspace
  login_unless_autologin "$auth_probe_status"

  cove_id="$(multitask_create_cove)"
  wave_id="$(multitask_create_wave "$cove_id")"
  printf 'Created cove: %s\nCreated wave: %s\n' "$cove_id" "$wave_id"
  multitask_poll_wave "$wave_id"
}
