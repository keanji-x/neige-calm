#!/usr/bin/env bash
# End-to-end demo for the M3-mcp-apps wire (post-M6): build the plugin,
# install it, enable it, tail events.
#
# Usage: ./demo.sh <wave-id>
#
# Assumes the kernel is already running on $NEIGE_PORT (default 3030). The
# wave id can be pulled from the UI or
#     curl localhost:$NEIGE_PORT/api/waves
# (pick any non-archived wave).
#
# Post-M6 flow:
#   1. Install + enable seeds the plugin process and registers its tools/list.
#   2. AddPanel in the UI shows `make_status_card` because the tool entry
#      carries `_meta.ui.resourceUri = ui://dev.neige.hello-world/status`.
#   3. Mounting the card hits the kernel's tools/call route which writes a
#      Card row with `kind = ui://...` (M2 path).
#   4. The iframe (status.html) renders, asks AppBridge for the card payload,
#      and the "Toggle overlay" button sends `tools/call neige.overlay.set`
#      through the host's M5 fan-out → kernel callbacks.rs → overlay event
#      on this SSE stream.
#
# NOTE on demo-wave config: slice B does not forward user_config into the
# plugin's env. The simplest path is to rewrite manifest.json's
# entrypoint.env.NEIGE_DEMO_WAVE before install — that's what this script
# does. See README.md for the rationale.

set -euo pipefail

NEIGE_PORT=${NEIGE_PORT:-3030}
PLUGIN_DIR="$(cd "$(dirname "$0")" && pwd)"
WAVE_ID="${1:-}"

if [ -z "$WAVE_ID" ]; then
  echo "Usage: $0 <wave-id>" >&2
  echo "Pick a wave id from the UI or 'curl localhost:$NEIGE_PORT/api/waves'." >&2
  exit 1
fi

echo "→ Building plugin binary..."
( cd "$PLUGIN_DIR" && cargo build --release --quiet )

mkdir -p "$PLUGIN_DIR/bin"
cp "$PLUGIN_DIR/target/release/hello-world" "$PLUGIN_DIR/bin/hello-world"

echo "→ Rewriting manifest.json with NEIGE_DEMO_WAVE=$WAVE_ID..."
python3 - "$PLUGIN_DIR/manifest.json" "$WAVE_ID" <<'PY'
import json, sys, pathlib
path, wave = sys.argv[1], sys.argv[2]
m = json.loads(pathlib.Path(path).read_text())
m.setdefault("entrypoint", {}).setdefault("env", {})["NEIGE_DEMO_WAVE"] = wave
pathlib.Path(path).write_text(json.dumps(m, indent=2) + "\n")
PY

echo "→ (Re)installing plugin..."
# Uninstall first so we can iterate cleanly. 404 is fine on first run.
curl -sS -X DELETE "localhost:$NEIGE_PORT/api/plugins/dev.neige.hello-world" \
  -o /dev/null -w "  delete: %{http_code}\n" || true

curl -sS -X POST "localhost:$NEIGE_PORT/api/plugins/install" \
  -H 'Content-Type: application/json' \
  -d "{\"source\":{\"kind\":\"local_path\",\"path\":\"$PLUGIN_DIR\"}}" \
  | (command -v jq >/dev/null && jq . || cat)

echo "→ Enabling plugin..."
curl -sS -X POST "localhost:$NEIGE_PORT/api/plugins/dev.neige.hello-world/enable" \
  | (command -v jq >/dev/null && jq . || cat)

echo "→ Tailing /api/events (Ctrl-C to stop)..."
curl -N -sS "localhost:$NEIGE_PORT/api/events"
