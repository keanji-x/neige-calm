#!/usr/bin/env bash
# End-to-end demo for the todo plugin: build, install, enable.
#
# Usage: ./demo.sh
#
# Assumes the kernel is already running on $NEIGE_PORT (default 3030).
#
# Flow:
#   1. cargo build --release
#   2. Copy target/release/todo into bin/todo (matches manifest.entrypoint.command).
#   3. (Re)install the plugin from this local path.
#   4. Enable it. The tool `make_todo_card` then shows up in AddPanel.
#   5. Mounting the card spawns the iframe which loads its items list from
#      the plugin KV (empty on first mount).
#
# Unlike plugins/hello-world/demo.sh this script takes no wave id — the todo
# view scopes itself per card, not per wave.

set -euo pipefail

NEIGE_PORT=${NEIGE_PORT:-3030}
PLUGIN_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ID="dev.neige.todo"

echo "-> Building plugin binary..."
( cd "$PLUGIN_DIR" && cargo build --release --quiet )

mkdir -p "$PLUGIN_DIR/bin"
cp "$PLUGIN_DIR/target/release/todo" "$PLUGIN_DIR/bin/todo"

echo "-> (Re)installing plugin..."
# Uninstall first so we can iterate cleanly. 404 is fine on first run.
curl -sS -X DELETE "localhost:$NEIGE_PORT/api/plugins/$PLUGIN_ID" \
  -o /dev/null -w "  delete: %{http_code}\n" || true

curl -sS -X POST "localhost:$NEIGE_PORT/api/plugins/install" \
  -H 'Content-Type: application/json' \
  -d "{\"source\":{\"kind\":\"local_path\",\"path\":\"$PLUGIN_DIR\"}}" \
  | (command -v jq >/dev/null && jq . || cat)

echo "-> Enabling plugin..."
curl -sS -X POST "localhost:$NEIGE_PORT/api/plugins/$PLUGIN_ID/enable" \
  | (command -v jq >/dev/null && jq . || cat)

echo "-> Done. Open the UI's AddPanel and pick 'Todo list card'."
