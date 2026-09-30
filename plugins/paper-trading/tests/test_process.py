"""Production stdio entry point and interactive operator, no real Agent/broker."""
import json
import os
from pathlib import Path
import pty
import queue
import select
import subprocess
import sys
import threading
import time

from .conftest import ROOT
from paper_trading.config import AccountConfig
from paper_trading.strategy import Portfolio


def account_values(rig):
    return {key: rig.values[key] for key in ('account_no', 'broker_home', 'cli_path', 'poll_seconds')}


class Host:
    def __init__(self, rig, data_dir=None):
        data_dir = data_dir or rig.data
        data_dir.mkdir(exist_ok=True)
        env = {"PATH": os.defpath, "HOME": str(rig.home), "LANG": "C.UTF-8",
               "NEIGE_PLUGIN_DATA_DIR": str(data_dir)}
        self.proc = subprocess.Popen([str(ROOT / "run")], cwd=data_dir, env=env,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.frames = queue.Queue()
        self.sequence = 0
        self.overlays = []
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()
        init = self.request("initialize", {"protocolVersion": "2025-11-25", "_meta": {
            "dev.neige/auth": {"expected_echo": "fixture-token"},
            "dev.neige/config": {"values": account_values(rig)}}})
        assert init["_meta"]["dev.neige/auth"]["echoed_token"] == "fixture-token"

    def read(self):
        for line in self.proc.stdout:
            self.frames.put(json.loads(line))

    def send(self, frame):
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()

    def receive(self):
        frame = self.frames.get(timeout=15)
        if frame.get("method") == "neige.overlay.set":
            self.overlays.append(frame["params"])
            self.send({"jsonrpc": "2.0", "id": frame["id"], "result": {"overlay_id": "fixture"}})
        return frame

    def request(self, method, params):
        self.sequence += 1
        key = self.sequence
        self.send({"jsonrpc": "2.0", "id": key, "method": method, "params": params})
        while True:
            frame = self.receive()
            if frame.get("id") == key and "method" not in frame:
                assert "error" not in frame, frame
                return frame["result"]

    def tool(self, name, args, track="track-owner"):
        params = {"name": name, "arguments": args}
        if track is not None:
            params["_meta"] = {"dev.neige/track": {"id": track}}
        return self.request("tools/call", params)

    def close(self):
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=5)
        self.thread.join(timeout=5)
        self.proc.stdout.close()
        self.proc.stderr.close()


def test_real_plugin_handshake_tools_track_fence_and_report_callbacks(rig):
    Portfolio(rig.data, AccountConfig.parse(account_values(rig)), rig.broker).import_legacy(rig.values)
    host = Host(rig)
    try:
        names = {t["name"] for t in host.request("tools/list", {})["tools"]}
        assert names == {"paper.ingest", "paper.decide", "paper.status", "paper.refresh",
                         "paper.pause", "paper.journal", "paper.review", "paper.strategy"}
        assert host.tool("paper.status", {}, track=None)["isError"]
        assert host.tool("paper.status", {}, track="other")["isError"]
        assert host.tool("paper.status", {"track_id": "track-owner"})["isError"]
        assert host.tool("paper.execute", {"code": "731"})["isError"]
        assert host.request("ping", {}) == {}
        source = host.tool("paper.ingest", {"week": "2026-09-21"})["structuredContent"]
        assert source["source_id"] == rig.source["source_id"]
        status = host.tool("paper.status", {})["structuredContent"]
        assert status["mode"] == "supervised_paper"
        while len({p["kind"] for p in host.overlays}) < 14:
            host.receive()
        assert {p["kind"] for p in host.overlays} == {
            "paper.strategy", "paper.portfolio", "paper.decisions", "paper.trades", "paper.alerts", "paper.journal", "paper.reviews",
            "paper.overview", "paper.activity", "paper.review_cards", "paper.strategy_details", "paper.order_details", "paper.trade_details", "paper.alert_details"}
        assert next(p for p in host.overlays if p['kind'] == 'paper.overview')['payload']['view'] == 'overview'
        assert all(p["entity_id"] == "track-owner" for p in host.overlays)
        # A legitimate digest or filesystem path can contain the three digits.
        assert "confirmation_code" not in json.dumps(host.overlays)
        assert not any(c[:2] in (["order", "buy"], ["order", "sell"], ["order", "cancel"])
                       for c in rig.calls())
        assert not any("--execute" in c for c in rig.calls())
    finally:
        host.close()


def test_operator_refuses_noninteractive_confirmation(rig, tmp_path):
    config = tmp_path / "config.json"
    config.write_text(json.dumps(account_values(rig)))
    result = subprocess.run([sys.executable, "-m", "paper_trading.operator", "--config", str(config),
                             "--data-dir", str(rig.data), "--decision", "entry-1"], cwd=ROOT,
                            input="731\n", text=True, capture_output=True, timeout=10)
    assert result.returncode == 2
    assert "interactive terminal required" in result.stderr
    assert rig.calls() == []


def test_operator_pty_displays_preview_then_waits_for_explicit_input(rig, tmp_path):
    rig.decide()
    Portfolio(rig.data, AccountConfig.parse(account_values(rig)), rig.broker).import_legacy(rig.values)
    state = rig.state()
    state["publish_on_submit"] = rig.order(rig.plan())
    rig.write(state)
    config = tmp_path / "config.json"
    config.write_text(json.dumps(account_values(rig)))
    # Only the clock is controlled. The real operator, broker subprocess, ledger,
    # terminal check and confirmation read run unchanged in the child process.
    driver = "from unittest.mock import patch; from datetime import datetime; from paper_trading.operator import main\n" \
             "with patch('paper_trading.strategy.utc_now', return_value=datetime.fromisoformat('2026-09-21T15:00:00+00:00')): main()"
    master, slave = pty.openpty()
    proc = subprocess.Popen([sys.executable, "-c", driver, "--config", str(config), "--data-dir", str(rig.data),
                             "--decision", "entry-1"], cwd=ROOT, stdin=slave, stdout=slave, stderr=slave)
    os.close(slave)
    output = b""
    try:
        deadline = time.monotonic() + 15
        while b"leave blank to stop:" not in output and time.monotonic() < deadline:
            if select.select([master], [], [], 1)[0]:
                output += os.read(master, 65536)
        assert b'"confirmation_code": "731"' in output, output
        assert b"leave blank to stop:" in output, output
        assert not any("--execute" in c for c in rig.calls())
        os.write(master, b"731\n")
        while proc.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                try:
                    output += os.read(master, 65536)
                except OSError:
                    break
        assert proc.wait(timeout=3) == 0, output
        assert len([c for c in rig.calls() if "--execute" in c]) == 1
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=3)
        os.close(master)
