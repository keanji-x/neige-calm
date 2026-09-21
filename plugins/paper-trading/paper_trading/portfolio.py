"""Decimal accounting from executions, not forecast highs or order acceptance."""
from decimal import Decimal
from fractions import Fraction

from .config import broker_money, integer, money, timestamp
from .ledger import digest

TERMINAL = {"settled", "canceled", "rejected", "expired", "recorded"}
BROKER_TERMINAL = {"Filled", "Canceled", "Rejected", "Expired", "PartialWithdrawal"}
BROKER_ACTIVE = {"NotReported", "New", "WaitToNew", "PartialFilled", "WaitToReplace",
                 "PendingReplace", "Replaced", "WaitToCancel", "PendingCancel"}


def cost_exposure(portfolio, decisions, fills):
    """Keep rational cost allocations exact at the risk-limit comparison."""
    entries = {d["id"]: d["broker_id"] for d in decisions}
    total = Fraction(0)
    for trade in portfolio:
        if not trade["quantity"]:
            continue
        order_id = entries[trade["entry_decision_id"]]
        cost = sum((Fraction(broker_money(f["price"])) * f["quantity"]
                    for f in fills if f["order_id"] == order_id), Fraction(0))
        total += cost * Fraction(trade["quantity"], trade["bought"])
    return total


def trades(decisions, fills):
    result = []
    for entry in decisions:
        plan = entry["body"]
        if plan["action"] != "buy":
            continue
        related = [d for d in decisions if d["body"]["trade_id"] == plan["trade_id"]
                   and d["body"]["action"] in ("buy", "sell")]
        sides = {d["broker_id"]: d["body"]["action"] for d in related if d["broker_id"]}
        executions = [f for f in fills if f["order_id"] in sides]
        buys = [f for f in executions if sides[f["order_id"]] == "buy"]
        sells = [f for f in executions if sides[f["order_id"]] == "sell"]
        buy_qty = sum(integer(f["quantity"]) for f in buys)
        sell_qty = sum(integer(f["quantity"]) for f in sells)
        if sell_qty > buy_qty:
            raise ValueError("sell executions exceed owned entry executions")
        cost = sum((broker_money(f["price"]) * integer(f["quantity"]) for f in buys), Decimal(0))
        proceeds = sum((broker_money(f["price"]) * integer(f["quantity"]) for f in sells), Decimal(0))
        average = cost / buy_qty if buy_qty else Decimal(0)
        if buys and sells and min(timestamp(f["time"]) for f in sells) < max(timestamp(f["time"]) for f in buys):
            raise ValueError("exit precedes completion of entry fills")
        gross = proceeds - average * sell_qty
        risk = (average - money(plan["stop_price"])) * buy_qty
        pending = any(d["state"] not in TERMINAL for d in related)
        value = {"trade_id": plan["trade_id"], "symbol": plan["symbol"],
                 "entry_decision_id": entry["id"], "bought": buy_qty, "sold": sell_qty,
                 "quantity": buy_qty - sell_qty, "average_entry": str(average),
                 "realized_gross_usd": str(gross), "net_pnl_usd": None,
                 "initial_price_risk_usd": str(risk),
                 "realized_r": str(gross / risk) if risk > 0 else None,
                 "stop_price": plan["stop_price"], "target_price": plan["target_price"],
                 "state": "pending" if pending else ("open" if buy_qty > sell_qty else
                           ("closed" if buy_qty else "not_entered")),
                 "evidence_revision": digest({"decisions": related, "fills": executions})}
        result.append(value)
    return result


def position_map(rows):
    result = {}
    for row in rows:
        name = row["symbol"]
        qty = integer(row["quantity"], zero=True)
        if row["currency"] != "USD" or name in result:
            raise ValueError("unsupported or duplicate broker position")
        if qty:
            result[name] = qty
    return result
