#!/usr/bin/env bash
set -euo pipefail
set -E

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR/.." rev-parse --show-toplevel)"
ENV_FILE="$REPO_ROOT/.env"
ARTIFACT_DIR="$REPO_ROOT/e2e-artifacts"

export NO_PROXY="127.0.0.1,localhost"
export no_proxy="$NO_PROXY"

# shellcheck source=e2e/lib/assert.sh
source "$SCRIPT_DIR/lib/assert.sh"
# shellcheck source=e2e/lib/stack.sh
source "$SCRIPT_DIR/lib/stack.sh"
# shellcheck source=e2e/lib/api.sh
source "$SCRIPT_DIR/lib/api.sh"

usage() {
  cat <<'EOF'
Usage: e2e/run.sh [--tier N | --all] [--case substring] [--list]

Defaults to tier 1 only. Tier 2 burns real Codex tokens and must be requested
with --tier 2 or --all.

Cases may call skip to exit with status 77; skipped cases do not fail the run.
EOF
}

on_err() {
  local status=$1 line=$2 command=$3
  printf 'ERR: line %s: %s\n' "$line" "$command" >&2
  return "$status"
}

case_metadata() {
  local case_file=$1
  (
    set -euo pipefail
    CASE_NAME=
    CASE_TIER=
    CASE_TIMEOUT_SECS=
    # shellcheck disable=SC1090
    source "$case_file"
    [[ -n "$CASE_NAME" ]] || fail "$case_file did not declare CASE_NAME"
    [[ -n "$CASE_TIER" ]] || fail "$case_file did not declare CASE_TIER"
    [[ -n "$CASE_TIMEOUT_SECS" ]] || fail "$case_file did not declare CASE_TIMEOUT_SECS"
    declare -F case_run >/dev/null || fail "$case_file did not define case_run"
    printf '%s\t%s\t%s\n' "$CASE_NAME" "$CASE_TIER" "$CASE_TIMEOUT_SECS"
  )
}

case_matches() {
  local case_file=$1 name=$2 tier=$3
  local basename
  basename="$(basename "$case_file")"

  if (( RUN_ALL == 0 )) && [[ "$tier" != "$TIER_FILTER" ]]; then
    return 1
  fi
  if [[ -n "$CASE_FILTER" && "$basename $name" != *"$CASE_FILTER"* ]]; then
    return 1
  fi
  return 0
}

run_case_with_timeout() {
  local timeout_secs=$1
  local timed_out_file="${TMPDIR:-/tmp}/neige-e2e-timeout-$RUN_ID"
  local case_pid watcher_pid status

  rm -f "$timed_out_file"
  case_run &
  case_pid=$!
  (
    sleep "$timeout_secs"
    if kill -0 "$case_pid" 2>/dev/null; then
      printf '1\n' >"$timed_out_file"
      kill "$case_pid" 2>/dev/null || true
    fi
  ) &
  watcher_pid=$!

  set +e
  wait "$case_pid"
  status=$?
  kill "$watcher_pid" 2>/dev/null
  wait "$watcher_pid" 2>/dev/null
  set -e

  if [[ -f "$timed_out_file" ]]; then
    rm -f "$timed_out_file"
    fail "case timed out after ${timeout_secs}s"
  fi
  rm -f "$timed_out_file"
  return "$status"
}

run_one_case() (
  set -euo pipefail
  set -E

  local case_file=$1 name=$2 tier=$3 timeout_secs=$4 case_status
  # shellcheck disable=SC1090
  source "$case_file"
  CASE_CHECK_SERVER_LOGS="${CASE_CHECK_SERVER_LOGS:-1}"

  RUN_ID="e2e-$(date +%s)-$RANDOM"
  DEV_ID="$RUN_ID"
  PROJECT="neige-calm-$DEV_ID"
  WORKSPACE="$E2E_CONTAINER_STATE_DIR/e2e-workspace"
  COOKIE_HEADER=""
  PORT=""
  SERVER_CID=""
  API_STATUS=""
  API_BODY=""
  AUTH_PROBE_STATUS=""

  trap 'on_err "$?" "$LINENO" "$BASH_COMMAND"' ERR
  trap e2e_cleanup EXIT

  cd "$REPO_ROOT" || exit 1
  printf 'CASE START %s tier=%s timeout=%ss\n' "$name" "$tier" "$timeout_secs"
  if (( tier >= 2 )); then
    stack_preflight 1
  else
    stack_preflight 0
  fi
  PORT="$(pick_port)"
  start_stack
  wait_for_health
  set +e
  run_case_with_timeout "$timeout_secs"
  case_status=$?
  set -e
  if (( case_status != 0 )); then
    exit "$case_status"
  fi
)

TIER_FILTER=1
RUN_ALL=0
CASE_FILTER=""
LIST_ONLY=0

while (($#)); do
  case "$1" in
    --tier)
      [[ $# -ge 2 ]] || fail "--tier requires a value"
      TIER_FILTER=$2
      RUN_ALL=0
      shift 2
      ;;
    --all)
      RUN_ALL=1
      shift
      ;;
    --case)
      [[ $# -ge 2 ]] || fail "--case requires a substring"
      CASE_FILTER=$2
      shift 2
      ;;
    --list)
      LIST_ONLY=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      fail "unknown argument: $1"
      ;;
  esac
done

mapfile -t CASE_FILES < <(find "$SCRIPT_DIR/cases" -maxdepth 1 -type f -name '[0-9][0-9][0-9]-*.sh' | sort)

selected=0
passed=0
skipped=0
failed=0
declare -a selected_files=()
declare -a selected_names=()
declare -a selected_tiers=()
declare -a selected_timeouts=()

for case_file in "${CASE_FILES[@]}"; do
  metadata="$(case_metadata "$case_file")"
  IFS=$'\t' read -r case_name case_tier case_timeout <<<"$metadata"
  if (( LIST_ONLY )); then
    printf '%s tier=%s timeout=%ss %s\n' "$(basename "$case_file" .sh)" "$case_tier" "$case_timeout" "$case_name"
    continue
  fi
  if case_matches "$case_file" "$case_name" "$case_tier"; then
    selected_files+=("$case_file")
    selected_names+=("$case_name")
    selected_tiers+=("$case_tier")
    selected_timeouts+=("$case_timeout")
  fi
done

if (( LIST_ONLY )); then
  exit 0
fi

for i in "${!selected_files[@]}"; do
  selected=$((selected + 1))
  case_id="$(basename "${selected_files[$i]}" .sh)"
  if run_one_case "${selected_files[$i]}" "${selected_names[$i]}" "${selected_tiers[$i]}" "${selected_timeouts[$i]}"; then
    passed=$((passed + 1))
    printf 'PASS %s tier=%s %s\n' "$case_id" "${selected_tiers[$i]}" "${selected_names[$i]}"
  else
    status=$?
    if (( status == E2E_SKIP_STATUS )); then
      skipped=$((skipped + 1))
      printf 'SKIP %s tier=%s status=%s %s\n' "$case_id" "${selected_tiers[$i]}" "$status" "${selected_names[$i]}"
    else
      failed=$((failed + 1))
      printf 'FAIL %s tier=%s status=%s %s\n' "$case_id" "${selected_tiers[$i]}" "$status" "${selected_names[$i]}"
    fi
  fi
done

printf 'SUMMARY selected=%s passed=%s skipped=%s failed=%s\n' "$selected" "$passed" "$skipped" "$failed"
(( failed == 0 ))
