#!/usr/bin/env bash
# shellcheck shell=bash

fail() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

: "${E2E_SKIP_STATUS:=77}"

skip() {
  printf 'SKIP: %s\n' "$*" >&2
  exit "$E2E_SKIP_STATUS"
}

poll_until() {
  local timeout_secs=$1
  local fn=$2
  shift 2

  local start=$SECONDS
  while (( SECONDS - start <= timeout_secs )); do
    if "$fn" "$@"; then
      return 0
    fi
    (( SECONDS - start < timeout_secs )) || break
    sleep "${POLL_INTERVAL_SECS:-2}"
  done

  return 1
}

file_in_container() {
  local path=$1
  [[ -n "${SERVER_CID:-}" ]] || fail "SERVER_CID is not set for docker exec"
  docker exec "$SERVER_CID" test -f "$path"
}
