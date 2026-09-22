"""Opt-in isolated host integration; no real account or agent credentials."""
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
    sys.path.insert(0, str(plugin))
    from paper_trading.broker import Broker
    from paper_trading.config import AccountConfig
    from paper_trading.strategy import Portfolio
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
        config = {"account_no": "FIXTURE-PAPER", "broker_home": str(home), "poll_seconds": 5,
                  "cli_path": str(plugin / "tests/fixture_cli.py")}
        request("/api/plugins/dev-neige-paper-trading/config", config, "PATCH")
        request("/api/plugins/dev-neige-paper-trading/enable", {}, "POST")
        data = root / 'plugin-data/dev-neige-paper-trading'
        portfolio = Portfolio(data, AccountConfig.parse(config), Broker(config['cli_path'], str(home), str(data)))
        proposal = portfolio.call(track['id'], 'paper.strategy', {
            'research_root': str(root / 'research'), 'symbols': ['SOXX.US'],
            'max_order_usd': '5000', 'max_portfolio_usd': '10000', 'max_trade_risk_usd': '100'})

        def check(phase, equity):
            for _ in range(150):
                overlays = request(f"/api/overlays?entity_kind=track&entity_id={track['id']}")
                overlays = [o for o in overlays if o['plugin_id'] == 'dev-neige-paper-trading']
                if len(overlays) == 14:
                    strategy = next(o for o in overlays if o['kind'] == 'paper.strategy')
                    overview = next(o for o in overlays if o['kind'] == 'paper.portfolio')
                    current = next(r['approved'] for r in strategy['payload']['rows'] if r['setting'] == 'Status')
                    balance = next(r['value'] for r in overview['payload']['rows'] if r['metric'] == 'Account equity (USD)')
                    if current == phase and balance == equity:
                        break
                time.sleep(0.1)
            else:
                raise AssertionError(f'plugin did not publish {phase} projections: {overlays}')
            metadata = {'base': base, 'track_id': track['id'], 'area_id': area['id'], 'version': version,
                        'phase': phase, 'equity': equity}
            (root / 'metadata.json').write_text(json.dumps(metadata, indent=2))
            subprocess.run(['node', str(plugin / 'tests/browser_smoke.cjs'), str(args.browser_package.resolve()),
                            str(root / 'metadata.json'), str(root / 'screenshots' / phase)], check=True, timeout=90)

        check('awaiting_approval', 'Unknown')
        assert not (home / 'calls.jsonl').exists(), 'proposal unexpectedly contacted broker'
        portfolio.approve(proposal['strategy']['proposal']['revision'])
        check('approved', '100000')
        calls = [json.loads(line) for line in (home / "calls.jsonl").read_text().splitlines()]
        assert not any("--execute" in call for call in calls)
        print(json.dumps({"result": "passed", "overlays": 14, "phases": ['awaiting_approval', 'approved'],
                          "host": version["buildSha"], "output": str(root)}))
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
