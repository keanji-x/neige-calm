#!/usr/bin/env bash

set -euo pipefail

require_tool() {
  local tool="${1:?tool name is required}"

  command -v "$tool" >/dev/null 2>&1 || {
    echo "::error::required tool '$tool' not found in PATH"
    exit 1
  }
}

require_path() {
  local path

  for path in "$@"; do
    [ -e "$path" ] || {
      echo "::error::required scan path does not exist: $path"
      exit 1
    }
  done
}

scan_must_be_empty() {
  local rule_label="${1:?rule label is required}"
  local output
  local rc
  shift

  set +e
  output="$("$@" 2>&1)"
  rc=$?
  set -e

  case "$rc" in
    0)
      printf '%s\n' "$output"
      echo "::error::$rule_label"
      return 1
      ;;
    1)
      return 0
      ;;
    *)
      printf '%s\n' "$output"
      echo "::error::$rule_label; scan infrastructure failed with exit $rc"
      return 1
      ;;
  esac
}
