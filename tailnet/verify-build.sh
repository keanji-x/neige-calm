#!/usr/bin/env bash
set -euo pipefail
[[ $# == 1 && -f "$1" ]] || { echo 'Usage: verify-build.sh <neige-tailnet binary>' >&2; exit 1; }
# Inspect the compiled artifact, not an environment variable or source manifest.
build_info="$(go version -m "$1")"
if ! awk '
  $1 == "build" && $2 ~ /^-tags=/ {
    sub(/^-tags=/, "", $2)
    count = split($2, tags, ",")
    for (i = 1; i <= count; i++) if (tags[i] == "ts_omit_logtail") found = 1
  }
  END { exit !found }
' <<< "$build_info"; then
  echo 'neige-tailnet release binary must be built with ts_omit_logtail' >&2
  exit 1
fi
