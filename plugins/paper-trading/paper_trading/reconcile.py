"""Fail-closed reconciliation and recovery; never submits or guesses an order."""
from datetime import timezone

from .config import integer, money, timestamp
from .ledger import digest, encoded
from .portfolio import BROKER_ACTIVE, BROKER_TERMINAL, position_map, trades


def remark(plan):
    return "nc-paper-" + digest(plan)[:32]


def verify_order(order, plan):
    if (order["symbol"] != plan["symbol"] or order["side"] != plan["action"].capitalize()
            or order["order_type"] != "LO" or integer(order["quantity"]) != plan["quantity"]
            or money(order["price"]) != money(plan["limit_price"])):
        raise ValueError("broker order does not match immutable decision")
    if "remark" in order and order["remark"] != remark(plan):
        raise ValueError("broker order remark mismatch")
    if order["status"] not in BROKER_ACTIVE | BROKER_TERMINAL:
        raise ValueError("unknown broker order status")


def refresh(db, ledger, config, broker, now):
    broker.identity(config.account_no)
    decisions = ledger.decisions(db)
    submitted = [d for d in decisions if d["broker_id"] or d["state"] in ("submitting", "unknown")]
    start = min((d["created_at"] for d in submitted), default=None)
    order_rows = (broker.orders(start) if start else []) + broker.orders()
    order_ids = {row["order_id"] for row in order_rows}
    if len(order_ids) > 500:
        raise ValueError("order history exceeds first-version reconciliation limit")
    order_ids.update(d["broker_id"] for d in submitted if d["broker_id"])
    details = {key: broker.order_detail(key) for key in sorted(order_ids)}
    for key, detail in details.items():
        if detail["order_id"] != key:
            raise ValueError("broker order detail identity mismatch")
    for decision in submitted:
        plan = decision["body"]
        order_id = decision["broker_id"]
        if not order_id:
            matches = [key for key, detail in details.items() if detail.get("remark") == remark(plan)]
            if len(matches) > 1:
                raise ValueError("multiple broker orders match one decision")
            if not matches:
                continue
            order_id = matches[0]
        detail = details[order_id]
        verify_order(detail, plan)
        ledger.change(db, decision["id"], decision["state"] if decision["broker_id"] else "working",
                      broker_id=order_id, broker_status=detail["status"])
    decisions = ledger.decisions(db)
    owned = {d["broker_id"]: d for d in decisions if d["broker_id"]}
    for key, detail in details.items():
        if detail["status"] not in BROKER_ACTIVE | BROKER_TERMINAL:
            raise ValueError("unknown broker order status")
        if key not in owned and detail["status"] not in BROKER_TERMINAL:
            raise ValueError("unowned active broker order; trading blocked")
    executions = (broker.executions(start) if start else []) + broker.executions()
    for raw in executions:
        order_id = raw["order_id"]
        if order_id not in owned:
            continue
        plan = owned[order_id]["body"]
        if raw["symbol"] != plan["symbol"]:
            raise ValueError("execution symbol mismatch")
        if not isinstance(raw["trade_id"], str) or not raw["trade_id"]:
            raise ValueError("broker execution id required")
        fill = {"trade_id": raw["trade_id"], "order_id": order_id, "symbol": raw["symbol"],
                "quantity": integer(raw["quantity"]), "price": str(money(str(raw["price"]))),
                "time": timestamp(raw["time"]).isoformat()}
        old = db.execute("SELECT body FROM fills WHERE id=?", (fill["trade_id"],)).fetchone()
        if old is not None and old[0] != encoded(fill):
            raise ValueError("conflicting broker execution id")
        if old is None:
            db.execute("INSERT INTO fills VALUES (?,?)", (fill["trade_id"], encoded(fill)))
            ledger.event(db, "fill", fill)
    fills = ledger.fills(db)
    for order_id, decision in owned.items():
        detail = details[order_id]
        count = sum(f["quantity"] for f in fills if f["order_id"] == order_id)
        requested = decision["body"]["quantity"]
        if count > requested or (detail["status"] == "Filled" and count != requested):
            raise ValueError("broker order/execution totals disagree; retry reconciliation")
        if detail["status"] == "PartialFilled" and not 0 < count < requested:
            raise ValueError("partial-fill total disagrees with order status")
        state = {"Filled": "settled", "Canceled": "canceled", "Rejected": "rejected",
                 "Expired": "expired", "PartialWithdrawal": "canceled"}.get(detail["status"], "working")
        ledger.change(db, decision["id"], state, broker_id=order_id, broker_status=detail["status"])
    portfolio = trades(ledger.decisions(db), fills)
    expected = {t["symbol"]: t["quantity"] for t in portfolio if t["quantity"]}
    if len(expected) != sum(t["quantity"] > 0 for t in portfolio):
        raise ValueError("overlapping symbol ownership")
    positions = broker.positions()
    if position_map(positions) != expected:
        raise ValueError("broker positions differ from ledger; trading blocked")
    assets = [a for a in broker.assets() if a["currency"] == "USD"]
    if len(assets) != 1:
        raise ValueError("exactly one USD account asset record required")
    cash = [c for c in assets[0]["cash_infos"] if c["currency"] == "USD"]
    if len(cash) != 1:
        raise ValueError("USD cash availability is missing")
    available = str(money(cash[0]["available_cash"], zero=True))
    equity = str(money(assets[0]["net_assets"], zero=True))
    broker.identity(config.account_no)
    snapshot = {"at": now.astimezone(timezone.utc).isoformat(), "available_cash_usd": available,
                "account_equity_usd": equity, "positions": positions,
                "unresolved": [d["id"] for d in ledger.decisions(db) if d["state"] in ("unknown", "submitting")]}
    ledger.set_meta(db, "snapshot", snapshot)
    ledger.set_meta(db, "error", None)
    return snapshot
