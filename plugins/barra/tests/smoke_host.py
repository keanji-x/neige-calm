"""Opt-in real-host smoke test, isolated data and disabled real Agent binaries.

Usage: python tests/smoke_host.py --server /path/calm-server --frontend /path/dist
       --prices /path/prices.csv --output /tmp/barra-host-smoke [--hold]
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, str(Path(__file__).parents[1]))
from barra.config import Config
from barra.runtime import Runtime


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--frontend", required=True, type=Path)
    parser.add_argument("--prices", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--hold", action="store_true")
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    plugin = Path(__file__).parents[1].resolve()
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    env = {"PATH": os.defpath, "HOME": str(root), "LANG": "C.UTF-8", "RUST_LOG": "error",
           "OPENBLAS_NUM_THREADS": "1"}
    flags = [str(args.server.resolve()), "--listen", f"127.0.0.1:{port}",
             "--db-url", "mock", "--data-dir", str(root / "data"),
             "--workspace-root", str(root / "workspaces"),
             "--plugins-dir", str(root / "plugins"), "--plugins-data-dir", str(root / "plugin-data"),
             "--codex-bin", "/bin/false", "--claude-bin", "/bin/false", "--auth-dev-autologin",
             "--fe-dist", str(args.frontend.resolve())]
    log = (root / "server.log").open("w")
    child = subprocess.Popen(flags, env=env, stdout=log, stderr=log, start_new_session=True)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(path, body=None, method=None):
        payload = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(base + path, data=payload, method=method,
                                     headers={"Content-Type": "application/json"})
        try:
            with opener.open(req, timeout=20) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"{path}: {error.code}: {error.read().decode()}") from error

    try:
        for _ in range(100):
            try:
                request("/api/version")
                break
            except (OSError, urllib.error.URLError):
                if child.poll() is not None:
                    raise RuntimeError("host exited; inspect server.log")
                time.sleep(0.2)
        request("/api/plugins/install", {"source": {"kind": "local_path", "path": str(plugin)}})
        request("/api/plugins/dev-neige-barra/config", {"prices_csv": str(args.prices.resolve())}, "PATCH")
        recipe = request("/api/track-recipes", {"title": "US Barra-style smoke", "body": (plugin / "recipe.md").read_text()})
        area = request("/api/areas", {"name": "Barra plugin smoke", "color": "#267A67"})
        track = request("/api/tracks", {"area_id": area["id"], "planner_provider": "codex", "title": "US Barra-style research", "recipe_id": recipe["id"],
                                        "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]}})
        # Seed only user configuration through the same start implementation.
        # The real host launches the plugin, which reads CSV and publishes via MCP.
        runtime = Runtime(root / "plugin-data" / "dev-neige-barra", lambda *args: None)
        runtime.start(track["id"], Config(update_hour_utc=0).json())
        request("/api/plugins/dev-neige-barra/enable", {}, "POST")
        for _ in range(150):
            overlays = request(f"/api/overlays?entity_kind=track&entity_id={track['id']}")
            status = next((o for o in overlays if o["kind"] == "barra.status"), None)
            if status:
                phase = next(r["value"] for r in status["payload"]["rows"] if r["metric"] == "本次运行")
                if phase in ("succeeded", "failed"):
                    assert phase == "succeeded", status
                    break
            time.sleep(0.2)
        else:
            raise AssertionError("plugin did not publish")
        assert {o["kind"] for o in overlays} >= {"barra.status", "barra.summary", "barra.exposures", "barra.history"}
        metadata = {"base": base, "track_id": track["id"], "area_id": area["id"], "recipe_id": recipe["id"]}
        (root / "metadata.json").write_text(json.dumps(metadata, indent=2))
        print(json.dumps(metadata), flush=True)
        if args.hold:
            while True:
                time.sleep(1)
    finally:
        os.killpg(child.pid, signal.SIGTERM)
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=5)
        log.close()


if __name__ == "__main__":
    main()
