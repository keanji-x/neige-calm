"""Native Report projections of the persisted trading ledger."""
import json
from datetime import datetime


def table(columns, rows, caption):
    return {"columns": [{"key": key, "label": label} for key, label in columns],
            "rows": rows, "caption": caption}


def tables(state):
    snapshot = state["snapshot"] or {}
    stamp = datetime.fromisoformat(snapshot["at"]).strftime("%Y-%m-%d %H:%M UTC") if snapshot else "Not yet reconciled"
    result = {
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
    if 'strategy' in state:
        strategy = state['strategy']
        rows = [{'setting': 'Status', 'approved': strategy['phase'], 'proposed': ''},
                {'setting': 'Paper account', 'approved': strategy['account_no'], 'proposed': ''}]
        active, proposal = strategy['active'], strategy['proposal']
        pending = proposal if proposal and (not active or proposal['revision'] != active['revision']) else None
        for key, label in [('track_id', 'Strategy Track'), ('revision', 'Revision')]:
            rows.append({'setting': label, 'approved': active[key] if active else '',
                         'proposed': pending[key] if pending else ''})
        for key, label in [('research_root', 'Research source'), ('symbols', 'Allowed symbols'),
                           ('max_order_usd', 'Maximum order (USD)'),
                           ('max_portfolio_usd', 'Maximum portfolio cost (USD)'),
                           ('max_trade_risk_usd', 'Maximum initial price risk (USD)'),
                           ('quote_max_age_seconds', 'Maximum quote age (seconds)'),
                           ('max_price_deviation_bps', 'Maximum price deviation (bps)')]:
            values = {'setting': label}
            for name, snapshot in [('approved', active), ('proposed', pending)]:
                value = snapshot['settings'][key] if snapshot else ''
                values[name] = ', '.join(value) if isinstance(value, list) else value
            rows.append(values)
        result = {'paper.strategy': table([('setting', 'Setting'), ('approved', 'Approved'),
                                            ('proposed', 'Awaiting approval')], rows,
                                          'Proposals do not authorize trading; approve the exact revision with the operator.'),
                  **result}
    return result
