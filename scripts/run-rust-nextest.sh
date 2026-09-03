#!/usr/bin/env bash
# Shared safe entry point for the broad Rust nextest suite. CI and local gates
# must call this script instead of assembling the command independently.
set -euo pipefail

cd "$(dirname "$0")/.."

if [ "$#" -eq 2 ] && [ "$1" = --test-threads ]; then
  if ! [[ "$2" =~ ^[1-9][0-9]*$ ]]; then
    echo "--test-threads requires a positive integer" >&2
    exit 2
  fi
elif [ "$#" -ne 0 ]; then
  echo "usage: scripts/run-rust-nextest.sh [--test-threads N]" >&2
  exit 2
fi

exec env -u NEIGE_CODEX_BIN \
  cargo nextest run --workspace --locked --features calm-server/codex-e2e \
    --profile ci "$@"
