#!/usr/bin/env bash
# Local dev — one-command launcher for calm-server + web (no docker).
#
#   scripts/dev.sh            # cargo run + npm run dev, in parallel
#   scripts/dev.sh --release  # build server with --release first
#   scripts/dev.sh --sqlite   # use a file-backed sqlite db instead of MockRepo
#
# Logs from both processes interleave on this terminal with [server]/[web]
# prefixes. Ctrl-C tears everything down.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CARGO_FLAGS=()
DB_URL="mock"
for arg in "$@"; do
  case "$arg" in
    --release) CARGO_FLAGS+=(--release) ;;
    --sqlite)
      mkdir -p "$ROOT/.dev-data"
      DB_URL="sqlite://$ROOT/.dev-data/calm.db?mode=rwc"
      ;;
    -h|--help)
      sed -n '2,11p' "$0"
      exit 0
      ;;
    *)
      echo "unknown flag: $arg" >&2
      exit 2
      ;;
  esac
done

# Ensure deps for the web before we race to start it.
if [ ! -d "$ROOT/web/node_modules" ]; then
  echo "[bootstrap] installing web deps…"
  (cd "$ROOT/web" && npm install)
fi

prefix() {
  local tag="$1"
  while IFS= read -r line; do
    printf '[%s] %s\n' "$tag" "$line"
  done
}

cleanup() {
  trap - INT TERM EXIT
  # Kill the whole process group so cargo's child + vite's child both go.
  pkill -P $$ 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup INT TERM EXIT

export CALM_LISTEN="${CALM_LISTEN:-127.0.0.1:4040}"
export CALM_DB_URL="${CALM_DB_URL:-$DB_URL}"
export CALM_ALLOWED_ORIGIN="${CALM_ALLOWED_ORIGIN:-http://localhost:5175}"
export RUST_LOG="${RUST_LOG:-info,calm_server=debug}"

echo "[dev] CALM_LISTEN=$CALM_LISTEN  CALM_DB_URL=$CALM_DB_URL"
echo "[dev] web on http://localhost:5175/calm/"

# Pipe both streams through prefix so logs don't tangle silently.
( cargo run "${CARGO_FLAGS[@]}" -p calm-server -- 2>&1 | prefix server ) &
( cd web && npm run dev -- --host 2>&1 | prefix web ) &

wait
