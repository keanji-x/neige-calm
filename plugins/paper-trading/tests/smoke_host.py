"""Opt-in isolated host integration; no real account or agent credentials."""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--frontend", required=True, type=Path)
    parser.add_argument("--browser-package", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    plugin = Path(__file__).parents[1].resolve()
    home = root / "broker-home"
    home.mkdir()
    (home / "broker.json").write_text(json.dumps({
        "identity": {"token": {"status": "valid", "dc_region": "ap"},
                     "account": {"account_no": "FIXTURE-PAPER", "account_channel": "lb_papertrading"}},
        "assets": [{"currency": "USD", "net_assets": "100000", "cash_infos": [
            {"currency": "USD", "available_cash": "100000"}]}],
        "positions": [], "orders": {}, "fills": [], "quotes": {}, "intraday": {}}))
    (root / "research").mkdir()
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    env = {"PATH": os.defpath, "HOME": str(root), "LANG": "C.UTF-8", "RUST_LOG": "error"}
    command = [str(args.server.resolve()), "--listen", f"127.0.0.1:{port}", "--db-url", "mock",
               "--data-dir", str(root / "data"), "--workspace-root", str(root / "workspaces"),
               "--plugins-dir", str(root / "plugins"), "--plugins-data-dir", str(root / "plugin-data"),
               "--codex-bin", "/bin/false", "--claude-bin", "/bin/false", "--auth-dev-autologin",
               "--fe-dist", str(args.frontend.resolve())]
    log = (root / "server.log").open("w")
    child = subprocess.Popen(command, env=env, stdout=log, stderr=log, start_new_session=True)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(path, body=None, method=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(base + path, data=data, method=method, headers={"Content-Type": "application/json"})
        try:
            with opener.open(req, timeout=20) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"{path}: {error.code}: {error.read().decode()}") from error

    try:
        for _ in range(100):
            try:
                version = request("/api/version")
                break
            except (OSError, urllib.error.URLError):
                if child.poll() is not None:
                    raise RuntimeError("isolated host exited; inspect server.log")
                time.sleep(0.1)
        else:
            raise RuntimeError("isolated host did not start")
        recipe = request("/api/track-recipes", {"title": "Paper portfolio", "body": (plugin / "recipe.md").read_text()})
        area = request("/api/areas", {"name": "Investment laboratory", "color": "#267A67"})
        track = request("/api/tracks", {"area_id": area["id"], "title": "Weekly paper strategy", "recipe_id": recipe["id"],
                                        "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]}})
        request("/api/plugins/install", {"source": {"kind": "local_path", "path": str(plugin)}})
        config = {"account_no": "FIXTURE-PAPER", "owner_track_id": track["id"], "broker_home": str(home),
                  "research_root": str(root / "research"), "symbols_json": '["SOXX.US"]', "max_order_usd": "5000",
                  "max_portfolio_usd": "10000", "max_trade_risk_usd": "100", "poll_seconds": 5,
                  "cli_path": str(plugin / "tests/fixture_cli.py")}
        request("/api/plugins/dev-neige-paper-trading/config", config, "PATCH")
        request("/api/plugins/dev-neige-paper-trading/enable", {}, "POST")
        for _ in range(150):
            overlays = request(f"/api/overlays?entity_kind=track&entity_id={track['id']}")
            if len(overlays) == 6:
                overview = next(o for o in overlays if o["kind"] == "paper.portfolio")
                assert next(r["value"] for r in overview["payload"]["rows"] if r["metric"] == "Account equity (USD)") == "100000"
                break
            time.sleep(0.1)
        else:
            raise AssertionError("plugin did not publish six report projections")
        metadata = {"base": base, "track_id": track["id"], "area_id": area["id"], "version": version}
        (root / "metadata.json").write_text(json.dumps(metadata, indent=2))
        subprocess.run(["node", str(plugin / "tests/browser_smoke.cjs"), str(args.browser_package.resolve()),
                        str(root / "metadata.json"), str(root / "screenshots")], check=True, timeout=90)
        calls = [json.loads(line) for line in (home / "calls.jsonl").read_text().splitlines()]
        assert not any("--execute" in call for call in calls)
        print(json.dumps({"result": "passed", "overlays": len(overlays), "host": version["buildSha"], "output": str(root)}))
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
