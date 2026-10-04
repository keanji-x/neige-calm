#!/usr/bin/env bash
# shellcheck shell=bash
# shellcheck disable=SC2154

CASE_NAME="daily Planner Track and report history"
CASE_TIER=1
CASE_TIMEOUT_SECS=300
CASE_CHECK_SERVER_LOGS=0

case_run() {
  local daily_json track_id date_value yesterday
  autologin_probe
  login_unless_autologin "$AUTH_PROBE_STATUS"
  expect_2xx GET /api/today/daily -
  daily_json="$API_BODY"
  track_id="$(json_get_string "$daily_json" track_id)"
  date_value="$(json_get_string "$daily_json" date)"
  [[ "$(json_get_string "$daily_json" time_zone)" == "Asia/Shanghai" ]] || fail "daily time zone is incorrect"
  expect_2xx GET "/api/today/daily?date=$date_value" -
  [[ "$(json_get_string "$API_BODY" track_id)" == "$track_id" ]] || fail "daily resolution changed its identity"
  expect_2xx GET "/api/tracks/$track_id" -
  printf '%s' "$API_BODY" | node -e '
const fs = require("fs");
const detail = JSON.parse(fs.readFileSync(0, "utf8"));
if (detail.track.closed_at !== null || detail.can_close !== false || detail.can_reopen !== false) {
  throw new Error("daily Track lifecycle is not kernel-owned and open");
}
if (!detail.cards.some(c => c.kind === "codex" && c.payload.planner_harness === true)
    || !detail.cards.some(c => c.kind === "track-report")) {
  throw new Error("daily Track has no usable Planner/report contract");
}
' || fail "daily Track contract is invalid"
  expect_2xx GET /api/track-templates -
  printf '%s' "$API_BODY" | node -e '
const rows = JSON.parse(require("fs").readFileSync(0, "utf8"));
if (rows.some(row => row.id === "daily-planner")) throw new Error("kernel template is publicly creatable");
' || fail "kernel template was exposed as a public create"
  yesterday="$(DAILY_DATE="$date_value" node -e 'const d = new Date(`${process.env.DAILY_DATE}T12:00:00Z`); d.setUTCDate(d.getUTCDate()-1); process.stdout.write(d.toISOString().slice(0,10));')"
  expect_2xx GET "/api/today/report-changes?date=$yesterday" -
  printf '%s' "$API_BODY" | node -e '
const page = JSON.parse(require("fs").readFileSync(0, "utf8"));
if (!Array.isArray(page.changes) || !Number.isInteger(page.through_event_id)
    || page.next_cursor !== null || page.time_zone !== "Asia/Shanghai") {
  throw new Error("report history page does not declare its snapshot");
}
' || fail "report history snapshot is invalid"
  printf 'Daily Planner read paths OK date=%s track=%s\n' "$date_value" "$track_id"
}
