"""Real stdio entry point against a host-callback harness, no real Agent."""
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import threading
import time

import pandas as pd

ROOT = Path(__file__).parents[1]


class Host:
    def __init__(self, tmp_path, prices):
        csv = tmp_path / "prices.csv"
        prices = prices.copy()
        # Match the real process clock without requiring a production-only clock override.
        prices.index = pd.bdate_range(end=pd.Timestamp.now(tz="America/New_York").date() - pd.Timedelta(days=1),
                                     periods=len(prices))
        prices.to_csv(csv, index_label="date")
        env = {k: os.environ[k] for k in ("PATH", "HOME", "LANG") if k in os.environ}
        env.update(NEIGE_PLUGIN_DATA_DIR=str(tmp_path / "data"), OPENBLAS_NUM_THREADS="1")
        self.stderr = (tmp_path / "stderr.log").open("w+")
        install = tmp_path / "install"
        shutil.copytree(ROOT, install, ignore=shutil.ignore_patterns(".venv", "__pycache__", ".pytest_cache", "tests"))
        interpreter = install / ".venv" / "bin" / "python"
        interpreter.parent.mkdir(parents=True)
        interpreter.symlink_to(sys.executable)
        data_dir = tmp_path / "data"
        data_dir.mkdir()
        manifest = json.loads((install / "manifest.json").read_text())
        self.child = subprocess.Popen([str(install / manifest["entrypoint"]["command"])], env=env,
                                      cwd=data_dir,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=self.stderr, text=True)
        self.frames = queue.Queue()
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.sequence = 0
        self.pushes = []
        reply = self.request("initialize", {"protocolVersion": "2025-11-25", "_meta": {
            "dev.neige/auth": {"expected_echo": "test-only-token"},
            "dev.neige/config": {"values": {"prices_csv": str(csv)}}}})
        assert reply["_meta"]["dev.neige/auth"]["echoed_token"] == "test-only-token"

    def read(self):
        for line in self.child.stdout:
            self.frames.put(json.loads(line))

    def send(self, frame):
        self.child.stdin.write(json.dumps(frame) + "\n")
        self.child.stdin.flush()

    def receive(self, timeout=10):
        frame = self.frames.get(timeout=timeout)
        if "method" in frame:
            assert frame["method"] == "neige.overlay.set"
            self.pushes.append(frame["params"])
            self.send({"jsonrpc": "2.0", "id": frame["id"], "result": {"overlay_id": "fixture"}})
        return frame

    def request(self, method, params):
        self.sequence += 1
        request_id = self.sequence
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            frame = self.receive()
            if frame.get("id") == request_id and "method" not in frame:
                assert "error" not in frame, frame
                return frame["result"]
        raise AssertionError("request timed out")

    def tool(self, name, args=None, track="track-a"):
        params = {"name": name, "arguments": args or {}}
        if track is not None:
            params["_meta"] = {"dev.neige/track": {"id": track}}
        return self.request("tools/call", params)

    def close(self):
        self.child.stdin.close()
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait(timeout=5)
        self.reader.join(timeout=5)
        self.child.stdout.close()
        self.stderr.close()


def test_real_stdio_start_calculate_publish_and_query(tmp_path, prices, config):
    host = Host(tmp_path, prices)
    try:
        tools = host.request("tools/list", {})["tools"]
        assert {t["name"] for t in tools} == {"barra.start", "barra.status", "barra.refresh", "barra.stop", "barra.series"}
        assert not host.tool("barra.start", config.json()).get("isError")
        assert host.request("ping", {}) == {}
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            reply = host.tool("barra.status")
            state = reply["structuredContent"]
            if state["phase"] in ("succeeded", "failed"):
                break
            time.sleep(0.02)
        assert state["phase"] == "succeeded", state
        assert state["latest"]["validation"]["observations"] == 60
        assert {p["kind"] for p in host.pushes} == {
            "barra.status", "barra.overview", "barra.leaders", "barra.validation", "barra.summary", "barra.exposures", "barra.history"}
        charts = host.tool("barra.series", {"series": ["BARRA:Beta", "BARRA:Forecast", "BARRA:EqualWeight"],
            "fields": ["close"], "period": "day", "mode": "live", "start": "2000-01-01", "as_of": "2099-01-01",
            "deadline_ms": int(time.time() * 1000) + 10000})
        assert all(s["status"] == "ok" and len(s["points"]) == 60 for s in charts["structuredContent"]["series"])
        assert host.tool("barra.series", {"series": []}, track="track-b")["isError"]
        assert all(p["entity_id"] == "track-a" for p in host.pushes)
        assert host.tool("barra.status", track="track-b")["isError"]
        assert host.tool("barra.stop")["structuredContent"]["enabled"] is False
    finally:
        host.close()


def test_real_stdio_rejects_missing_or_argument_track_context(tmp_path, prices, config):
    host = Host(tmp_path, prices)
    try:
        assert host.tool("barra.start", config.json(), track=None)["isError"]
        assert host.tool("barra.start", config.json() | {"track_id": "victim"})["isError"]
        assert host.tool("barra.status")["isError"]
        assert not host.pushes
    finally:
        host.close()
