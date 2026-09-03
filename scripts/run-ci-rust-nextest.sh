#!/usr/bin/env bash
# CI dispatch for the shared Rust nextest wrapper. Keep runner policy here so
# both hosted and self-hosted branches are executable in the hermetic selftest.
set -euo pipefail

cd "$(dirname "$0")/.."

usage='usage: scripts/run-ci-rust-nextest.sh {github-hosted|self-hosted} [--partition KIND:N/M]'
if [ "$#" -ne 1 ] && { [ "$#" -ne 3 ] || [ "${2:-}" != --partition ]; }; then
  echo "$usage" >&2
  exit 2
fi

runner="$1"
shift
case "$runner" in
  github-hosted)
    exec scripts/run-rust-nextest.sh "$@"
    ;;
  self-hosted)
    exec scripts/run-rust-nextest.sh --test-threads 8 "$@"
    ;;
  *)
    echo "$usage" >&2
    exit 2
    ;;
esac
