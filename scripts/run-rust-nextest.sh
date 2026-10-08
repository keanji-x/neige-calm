#!/usr/bin/env bash
# Shared safe entry point for the broad Rust nextest suite. CI and local gates
# must call this script instead of assembling the command independently.
set -euo pipefail

cd "$(dirname "$0")/.."

usage='usage: scripts/run-rust-nextest.sh [--archive-file FILE] [--test-threads N] [--partition KIND:N/M]'
# Without --archive-file the suite is built here. With it, the tests come from a
# `cargo nextest archive` built by CI's rust-build job with the same features,
# extracted into ./target so compile-time CARGO_BIN_EXE_* paths still resolve.
source_args=(--workspace --locked --features calm-server/codex-e2e)
args=()
guard_args=()
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
    --archive-file)
      if [ "$#" -lt 2 ] || [ ! -f "$2" ]; then
        echo "--archive-file requires an existing file" >&2
        exit 2
      fi
      guard_args=(--archive-file "$2")
      source_args=(--archive-file "$2" --workspace-remap . --extract-to . --extract-overwrite)
      shift 2
      ;;
    *)
      echo "$usage" >&2
      exit 2
      ;;
  esac
done

# Validate the complete CI inventory before partitioning. Archive guard listings
# extract to private temporary directories; only the run below writes ./target.
env -u NEIGE_CODEX_BIN python3 scripts/ci/check-nextest-overrides.py "${guard_args[@]}"

exec env -u NEIGE_CODEX_BIN \
  cargo nextest run "${source_args[@]}" --profile ci "${args[@]}"
