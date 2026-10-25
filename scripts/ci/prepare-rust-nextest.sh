#!/usr/bin/env bash
# Build a complete nextest input, then guard that same input before CI uploads it.
# Event, profile environment and cache policy belong to the workflow.
set -euo pipefail

cd "$(dirname "$0")/../.."
usage='usage: scripts/ci/prepare-rust-nextest.sh {archive --archive-file FILE|inventory}'
source_args=(--workspace --locked --features calm-server/codex-e2e,calm-server/fixtures --profile ci)
case "${1:-}" in
  archive)
    if [ "$#" -ne 3 ] || [ "$2" != --archive-file ] || [ -z "$3" ]; then
      echo "$usage" >&2
      exit 2
    fi
    env -u NEIGE_CODEX_BIN cargo nextest archive "${source_args[@]}" --archive-file "$3" --timings
    env -u NEIGE_CODEX_BIN python3 scripts/ci/check-nextest-overrides.py --archive-file "$3"
    ;;
  inventory)
    if [ "$#" -ne 1 ]; then
      echo "$usage" >&2
      exit 2
    fi
    env -u NEIGE_CODEX_BIN cargo nextest list "${source_args[@]}" \
      --list-type binaries-only --message-format json > /dev/null
    env -u NEIGE_CODEX_BIN python3 scripts/ci/check-nextest-overrides.py
    ;;
  *)
    echo "$usage" >&2
    exit 2
    ;;
esac
