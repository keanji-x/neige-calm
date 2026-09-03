#!/usr/bin/env bash
# Shared safe entry point for the broad Rust nextest suite. CI and local gates
# must call this script instead of assembling the command independently.
set -euo pipefail

cd "$(dirname "$0")/.."

usage='usage: scripts/run-rust-nextest.sh [--test-threads N] [--partition KIND:N/M]'
args=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --test-threads)
      if [ "$#" -lt 2 ] || ! [[ "$2" =~ ^[1-9][0-9]*$ ]]; then
        echo "--test-threads requires a positive integer" >&2
        exit 2
      fi
      args+=("$1" "$2")
      shift 2
      ;;
    --partition)
      if [ "$#" -lt 2 ] || ! [[ "$2" =~ ^(hash|count|slice):[1-9][0-9]*/[1-9][0-9]*$ ]]; then
        echo "--partition requires KIND:N/M" >&2
        exit 2
      fi
      partition_numbers="${2#*:}"
      partition_index="${partition_numbers%/*}"
      partition_total="${partition_numbers#*/}"
      if [ "$partition_index" -gt "$partition_total" ]; then
        echo "--partition requires N <= M" >&2
        exit 2
      fi
      args+=("$1" "$2")
      shift 2
      ;;
    *)
      echo "$usage" >&2
      exit 2
      ;;
  esac
done

exec env -u NEIGE_CODEX_BIN \
  cargo nextest run --workspace --locked --features calm-server/codex-e2e \
    --profile ci "${args[@]}"
