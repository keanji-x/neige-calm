from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import sys

import pytest

ROOT = Path(__file__).parents[1]
sys.path.insert(0, str(ROOT))
from paper_trading.broker import Broker
from paper_trading.config import Config
from paper_trading.engine import Engine
from paper_trading.reconcile import remark

NOW = datetime(2026, 9, 21, 15, 0, tzinfo=timezone.utc)


@pytest.fixture
def rig(tmp_path):
    home = tmp_path / "home"
    home.mkdir()
    data = tmp_path / "data"
    data.mkdir()
    research = tmp_path / "research"
    week = research / "weekly/2026/09_21"
    week.mkdir(parents=True)
    (week / "weekly_market_analysis_cn.md").write_text("# Source report\nA long research thesis, not an instruction.\n")
    predictions = [{"id": "09_21-p03", "issued": "2026-09-21", "symbol": "SOXX", "direction": "long",
                    "conviction_tier": "satellite", "horizon_end": "2026-09-25"}]
    (research / "weekly/2026/predictions.jsonl").write_text("\n".join(json.dumps(p) for p in predictions))
    values = {"account_no": "PAPER123", "owner_track_id": "track-owner", "broker_home": str(home),
              "research_root": str(research), "symbols_json": '["SOXX.US"]', "max_order_usd": "5000",
              "max_portfolio_usd": "10000", "max_trade_risk_usd": "100",
              "cli_path": str(ROOT / "tests/fixture_cli.py"), "poll_seconds": 5}
    state = {"identity": {"token": {"status": "valid", "dc_region": "ap"},
                           "account": {"account_no": "PAPER123", "account_channel": "lb_papertrading"}},
             "assets": [{"currency": "USD", "net_assets": "100000", "cash_infos": [
                 {"currency": "USD", "available_cash": "100000"}]}],
             "positions": [], "orders": {}, "fills": [], "next_response": {"order_id": "order-1"},
             "quotes": {"SOXX.US": {"symbol": "SOXX.US", "last": "100.00", "status": "Normal"}},
             "intraday": {"SOXX.US": [{"time": NOW.isoformat(), "price": "100.00"}]}}
    (home / "broker.json").write_text(json.dumps(state))

    class Rig:
        def __init__(self):
            self.home, self.data, self.research = home, data, research
            self.values = values
            self.config = Config.parse(values)
            self.broker = Broker(values["cli_path"], str(home), str(data))
            self.engine = Engine(data, self.config, self.broker, clock=lambda: NOW)
            self.source = self.engine.call("track-owner", "paper.ingest", {"week": "2026-09-21"})

        def state(self):
            return json.loads((home / "broker.json").read_text())

        def write(self, state):
            (home / "broker.json").write_text(json.dumps(state))

        def calls(self):
            path = home / "calls.jsonl"
            return [json.loads(s) for s in path.read_text().splitlines()] if path.exists() else []

        def plan(self, **changes):
            return {"decision_id": "entry-1", "cycle_id": "cycle-1", "source_id": self.source["source_id"],
                    "prediction_id": "09_21-p03", "trade_id": "trade-1", "action": "buy", "symbol": "SOXX.US",
                    "rationale": "Source thesis confirmed against current market data.",
                    "valid_until": (NOW + timedelta(hours=1)).isoformat(), "quantity": 10,
                    "limit_price": "100.00", "stop_price": "95.00", "target_price": "110.00", **changes}

        def decide(self, plan=None):
            return self.engine.call("track-owner", "paper.decide", plan or self.plan())

        def order(self, plan, key="order-1", status="New"):
            return {"order_id": key, "symbol": plan["symbol"], "side": plan["action"].capitalize(),
                    "order_type": "LO", "status": status, "quantity": str(plan["quantity"]),
                    "price": plan["limit_price"], "remark": remark(plan)}

        def submit(self, plan, key="order-1", lost=False):
            state = self.state()
            state["next_response"] = {"order_id": key}
            state["publish_on_submit"] = self.order(plan, key)
            state["fail_after_submit"] = lost
            self.write(state)
            args, preview = self.engine.operator_preview(plan["decision_id"])
            assert "731" in preview
            return self.engine.operator_confirm(plan["decision_id"], args, "731")

        def fill(self, order_id, qty, price="100.00", fill_id="fill-1", remaining=10, status="Filled"):
            state = self.state()
            state["orders"][order_id]["status"] = status
            state["fills"].append({"trade_id": fill_id, "order_id": order_id, "symbol": "SOXX.US",
                                    "quantity": str(qty), "price": price, "time": NOW.isoformat()})
            state["positions"] = ([{"symbol": "SOXX.US", "quantity": str(remaining), "available": str(remaining),
                                    "currency": "USD", "cost_price": "100.00"}] if remaining else [])
            self.write(state)
            return self.engine.process_once()

    return Rig()
