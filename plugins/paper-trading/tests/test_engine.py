from datetime import timedelta
import json

import pytest

from paper_trading.broker import BrokerError
from paper_trading.config import Config
from paper_trading.engine import Engine
from .conftest import NOW


def test_paper_entry_partial_restart_exit_and_review(rig):
    plan = rig.plan()
    assert rig.decide(plan)["state"] == "queued"
    assert rig.engine.process_once()["decisions"][0]["state"] == "ready"
    assert not any("--execute" in c for c in rig.calls())
    assert rig.submit(plan)["decisions"][0]["state"] == "working"
    partial = rig.fill("order-1", 4, remaining=4, status="PartialFilled")
    assert partial["error"] is None
    assert partial["trades"][0]["quantity"] == 4
    rig.engine = Engine(rig.data, rig.config, rig.broker, clock=lambda: NOW)
    complete = rig.fill("order-1", 6, fill_id="fill-2", remaining=10)
    assert complete["decisions"][0]["state"] == "settled"
    assert complete["trades"][0]["quantity"] == 10
    assert len(rig.engine.process_once()["fills"]) == 2
    exit_plan = rig.plan(decision_id="exit-1", action="sell", quantity=4, limit_price="105.00")
    del exit_plan["stop_price"], exit_plan["target_price"]
    state = rig.state()
    state["quotes"]["SOXX.US"]["last"] = "105.00"
    state["intraday"]["SOXX.US"][0]["price"] = "105.00"
    rig.write(state)
    rig.decide(exit_plan)
    rig.submit(exit_plan, "order-2")
    partial_exit = rig.fill("order-2", 4, price="105.00", fill_id="fill-3", remaining=6)
    assert partial_exit["trades"][0]["realized_gross_usd"] == "20.00"
    assert partial_exit["trades"][0]["quantity"] == 6
    exit_plan = exit_plan | {"decision_id": "exit-2", "quantity": 6}
    rig.decide(exit_plan)
    rig.submit(exit_plan, "order-3")
    closed = rig.fill("order-3", 6, price="105.00", fill_id="fill-4", remaining=0)
    trade = closed["trades"][0]
    assert trade["state"] == "closed"
    assert trade["realized_gross_usd"] == "50.00"
    assert trade["realized_r"] == "1"
    assert trade["net_pnl_usd"] is None
    review = {"review_id": "review-1", "trade_id": "trade-1", "evidence_revision": trade["evidence_revision"],
              "analysis": "Actual fills produced gross USD 50; fees remain unavailable.",
              "next_action": "Keep the original risk limits and inspect the next report."}
    assert rig.engine.call("track-owner", "paper.review", review) == review
    assert rig.engine.call("track-owner", "paper.review", review) == review
    status = rig.engine.call("track-owner", "paper.status", {})
    assert len(status["reviews"]) == 1
    assert [d["body"] for d in status["decisions"]][0] == plan
    assert len([c for c in rig.calls() if "--execute" in c]) == 3


def test_decision_retry_is_idempotent_and_changed_payload_is_refused(rig):
    assert rig.decide() == rig.decide()
    with pytest.raises(ValueError, match="different immutable"):
        rig.decide(rig.plan(quantity=11))
    assert len(rig.engine.call("track-owner", "paper.status", {})["decisions"]) == 1


def test_no_repeat_submit_after_lost_ack(rig):
    rig.decide()
    with pytest.raises(ValueError, match="outcome unknown"):
        rig.submit(rig.plan(), lost=True)
    status = rig.engine.call("track-owner", "paper.status", {})
    assert status["decisions"][0]["state"] == "unknown"
    with pytest.raises(ValueError, match="not eligible"):
        rig.engine.operator_preview("entry-1")
    status = rig.engine.process_once()
    assert status["decisions"][0]["broker_id"] == "order-1"
    with pytest.raises(ValueError, match="not eligible"):
        rig.engine.operator_preview("entry-1")
    assert len([c for c in rig.calls() if "--execute" in c]) == 1


def test_unknown_without_remark_is_not_guessed_or_resubmitted(rig):
    rig.decide()
    with pytest.raises(ValueError):
        rig.submit(rig.plan(), lost=True)
    state = rig.state()
    del state["orders"]["order-1"]["remark"]
    rig.write(state)
    status = rig.engine.process_once()
    assert "unowned active" in status["error"]
    assert status["decisions"][0]["state"] == "unknown"
    assert status["decisions"][0]["broker_id"] is None
    assert len([c for c in rig.calls() if "--execute" in c]) == 1


@pytest.mark.parametrize("track", [None, "", "other-track"])
def test_track_identity_fence(rig, track):
    with pytest.raises(ValueError, match="does not own"):
        rig.engine.call(track, "paper.decide", rig.plan())
    assert rig.calls() == []


def test_tool_cannot_supply_track_or_execute_confirmation(rig):
    with pytest.raises(ValueError, match="unknown fields"):
        rig.decide(rig.plan(track_id="other-track"))
    with pytest.raises(ValueError, match="unknown tool"):
        rig.engine.call("track-owner", "paper.execute", {"code": "731"})


@pytest.mark.parametrize("field,value", [("account_channel", "lb_live"), ("account_no", "OTHER")])
def test_account_fence_blocks_operator_before_preview(rig, field, value):
    rig.decide()
    state = rig.state()
    state["identity"]["account"][field] = value
    rig.write(state)
    with pytest.raises(BrokerError, match="identity check"):
        rig.engine.operator_preview("entry-1")
    assert not any(len(c) > 1 and c[0] == "order" and c[1] in ("buy", "sell") for c in rig.calls())


def test_account_switch_after_preview_is_refused(rig):
    rig.decide()
    args, _ = rig.engine.operator_preview("entry-1")
    state = rig.state()
    state["identity"]["account"]["account_channel"] = "lb_live"
    rig.write(state)
    with pytest.raises(BrokerError):
        rig.engine.operator_confirm("entry-1", args, "731")
    assert not any("--execute" in c for c in rig.calls())


@pytest.mark.parametrize("change", [{"quantity": True}, {"quantity": 1.5}, {"quantity": "10"},
    {"limit_price": "NaN"}, {"limit_price": "Infinity"}, {"limit_price": "1e2"},
    {"limit_price": "0.50"}, {"stop_price": "101"}, {"target_price": "99"}, {"symbol": "--help"},
    {"valid_until": "2026-09-21T15:30:00"}, {"valid_until": NOW.isoformat()}])
def test_invalid_decisions_do_not_enter_ledger(rig, change):
    with pytest.raises(ValueError):
        rig.decide(rig.plan(**change))
    assert rig.engine.call("track-owner", "paper.status", {})["decisions"] == []


@pytest.mark.parametrize("field,value", [("direction", "short"), ("conviction_tier", "watch")])
def test_non_executable_research_is_not_promoted(rig, field, value):
    path = rig.research / "weekly/2026/predictions.jsonl"
    prediction = json.loads(path.read_text()) | {field: value}
    path.write_text(json.dumps(prediction))
    rig.source = rig.engine.call("track-owner", "paper.ingest", {"week": "2026-09-21"})
    with pytest.raises(ValueError, match="non-watch"):
        rig.decide()


def test_stale_quote_refuses_readiness_and_native_preview(rig):
    rig.decide()
    state = rig.state()
    state["intraday"]["SOXX.US"][0]["time"] = (NOW - timedelta(hours=1)).isoformat()
    rig.write(state)
    status = rig.engine.process_once()
    assert status["decisions"][0]["state"] == "queued"
    assert "stale" in status["decisions"][0]["error"]
    with pytest.raises(ValueError, match="stale"):
        rig.engine.operator_preview("entry-1")


def test_external_position_blocks_trading(rig):
    rig.decide()
    state = rig.state()
    state["positions"] = [{"symbol": "SOXX.US", "quantity": "1", "available": "1", "currency": "USD"}]
    rig.write(state)
    assert "positions differ" in rig.engine.process_once()["error"]
    with pytest.raises(ValueError, match="positions differ"):
        rig.engine.operator_preview("entry-1")


def test_pause_prevents_new_entries_and_preserves_accounting(rig):
    rig.decide()
    rig.engine.call("track-owner", "paper.pause", {"paused": True})
    assert rig.engine.process_once()["snapshot"] is not None
    with pytest.raises(ValueError, match="paused"):
        rig.engine.operator_preview("entry-1")


def test_risk_and_cash_limits_are_program_enforced(rig):
    rig.decide(rig.plan(quantity=30))
    status = rig.engine.process_once()
    assert "price risk" in status["decisions"][0]["error"]
    state = rig.state()
    state["assets"][0]["cash_infos"][0]["available_cash"] = "1"
    rig.write(state)
    rig.engine.config = Config.parse(rig.values | {"max_trade_risk_usd": "1000"})
    assert "cash is insufficient" in rig.engine.process_once()["decisions"][0]["error"]


def test_conflicting_execution_id_rolls_back_entire_snapshot(rig):
    rig.decide()
    rig.submit(rig.plan())
    rig.fill("order-1", 10)
    state = rig.state()
    state["fills"][0]["price"] = "99.00"
    rig.write(state)
    failed = rig.engine.process_once()
    assert "conflicting broker execution" in failed["error"]
    assert failed["fills"][0]["price"] == "100.00"


def test_source_snapshot_remains_immutable_when_file_changes(rig):
    old = rig.source
    report = rig.research / "weekly/2026/09_21/weekly_market_analysis_cn.md"
    report.write_text("# Rewritten source\nDifferent evidence.")
    new = rig.engine.call("track-owner", "paper.ingest", {"week": "2026-09-21"})
    assert new["source_id"] != old["source_id"]
    rig.decide(rig.plan(source_id=old["source_id"]))
    assert rig.engine.call("track-owner", "paper.status", {})["decisions"][0]["body"]["source_id"] == old["source_id"]


def test_research_symlink_escape_is_rejected(rig, tmp_path):
    outside = tmp_path / "outside.md"
    outside.write_text("private material")
    report = rig.research / "weekly/2026/09_21/weekly_market_analysis_cn.md"
    report.unlink()
    report.symlink_to(outside)
    with pytest.raises(ValueError, match="escapes"):
        rig.engine.call("track-owner", "paper.ingest", {"week": "2026-09-21"})


def test_ledger_cannot_be_rebound(rig):
    with pytest.raises(ValueError, match="cannot be changed"):
        Engine(rig.data, Config.parse(rig.values | {"owner_track_id": "other"}), rig.broker)


def test_restart_marks_crashed_submission_unknown(rig):
    rig.decide()
    with rig.engine.ledger.session() as db:
        rig.engine.ledger.change(db, "entry-1", "submitting")
    restarted = Engine(rig.data, rig.config, rig.broker, clock=lambda: NOW)
    assert restarted.call("track-owner", "paper.status", {})["decisions"][0]["state"] == "unknown"


def test_cancel_uses_native_confirmation_and_does_not_invent_fills(rig):
    rig.decide()
    rig.submit(rig.plan())
    state = rig.state()
    state["next_response"] = "cancel accepted"
    state["publish_on_submit"] = state["orders"]["order-1"] | {"status": "Canceled"}
    rig.write(state)
    args, preview = rig.engine.operator_preview("entry-1", cancel=True)
    assert "731" in preview
    result = rig.engine.operator_confirm("entry-1", args, "731", cancel=True)
    assert result["decisions"][0]["state"] == "canceled"
    assert result["fills"] == []


def test_unchanged_reconciliation_does_not_append_false_transitions(rig):
    rig.decide()
    rig.submit(rig.plan())
    first = rig.fill("order-1", 10)
    second = rig.engine.process_once()
    assert second["journal"] == first["journal"]
    assert second["trades"] == first["trades"]


def test_invalid_confirmation_does_not_poison_submission_state(rig):
    rig.decide()
    args, _ = rig.engine.operator_preview("entry-1")
    with pytest.raises(ValueError, match="confirmation"):
        rig.engine.operator_confirm("entry-1", args, "not-a-code")
    status = rig.engine.call("track-owner", "paper.status", {})
    assert status["decisions"][0]["state"] in ("queued", "ready")
    assert not any("--execute" in c for c in rig.calls())
