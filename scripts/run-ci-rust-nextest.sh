#!/usr/bin/env bash
# CI dispatch for the shared Rust nextest wrapper. Keep runner policy here so
# both hosted and self-hosted branches are executable in the hermetic selftest.
set -euo pipefail

cd "$(dirname "$0")/.."

usage='usage: scripts/run-ci-rust-nextest.sh {github-hosted|self-hosted}'
if [ "$#" -ne 1 ]; then
  echo "$usage" >&2
  exit 2
fi

case "$1" in
  github-hosted)
    exec scripts/run-rust-nextest.sh
    ;;
  self-hosted)
    exec scripts/run-rust-nextest.sh --test-threads 4
    ;;
  *)
    echo "$usage" >&2
    exit 2
    ;;
esac
