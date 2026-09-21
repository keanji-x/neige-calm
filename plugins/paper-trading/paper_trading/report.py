"""Native Report projections of the persisted trading ledger."""
import json
from datetime import datetime


def table(columns, rows, caption):
    return {"columns": [{"key": key, "label": label} for key, label in columns],
            "rows": rows, "caption": caption}


def tables(state):
    snapshot = state["snapshot"] or {}
    stamp = datetime.fromisoformat(snapshot["at"]).strftime("%Y-%m-%d %H:%M UTC") if snapshot else "Not yet reconciled"
    return {
        "paper.portfolio": table([("metric", "Metric"), ("value", "Value")], [
            {"metric": "Mode", "value": "Supervised paper trading"},
            {"metric": "New entries", "value": "Paused" if state["paused"] else "Enabled"},
            {"metric": "Account equity (USD)", "value": snapshot.get("account_equity_usd", "Unknown")},
            {"metric": "Available cash (USD)", "value": snapshot.get("available_cash_usd", "Unknown")},
            {"metric": "Reconciliation", "value": state["error"] or ("Current snapshot" if snapshot else "Pending")},
        ], stamp),
        "paper.decisions": table([("id", "Decision"), ("symbol", "Symbol"), ("action", "Action"),
                                   ("quantity", "Shares"), ("price", "Limit"), ("state", "State"),
                                   ("order", "Broker order"), ("error", "Attention")], [
            {"id": d["id"], "symbol": d["body"]["symbol"], "action": d["body"]["action"],
             "quantity": d["body"].get("quantity", ""), "price": d["body"].get("limit_price", ""),
             "state": d["state"], "order": d["broker_id"] or "", "error": d["error"] or ""}
            for d in state["decisions"][-200:]], stamp),
        "paper.trades": table([("trade_id", "Trade"), ("symbol", "Symbol"), ("quantity", "Shares"),
                                ("average_entry", "Average entry"), ("realized_gross_usd", "Realized gross USD"),
                                ("state", "State")], state["trades"][-200:], "Realized gross P/L; fees excluded"),
        "paper.alerts": table([("trade_id", "Trade"), ("reason", "Attention")], state["alerts"],
                               "Alerts only; no broker stop orders"),
        "paper.journal": table([("seq", "Event"), ("at", "Time"), ("kind", "Kind"), ("detail", "Detail")],
                                [{"seq": e["seq"], "at": e["at"], "kind": e["kind"],
                                  "detail": json.dumps(e["body"], ensure_ascii=False)} for e in state["journal"]], stamp),
        "paper.reviews": table([("trade_id", "Trade"), ("analysis", "Review"), ("next_action", "Next action")],
                                state["reviews"][-200:], "Reviews of closed trades"),
    }
