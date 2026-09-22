"""Native Report projections of the persisted trading ledger."""
from datetime import datetime

from .report_text import event_text
from .report_views import view_payloads


def display_cell(value):
    # Native table cells allow 2048 Unicode code points, including any JSON
    # punctuation in journal details. Ledger and tool responses stay complete.
    suffix = '... [truncated]'
    if isinstance(value, str) and len(value) > 2048:
        return value[:2048 - len(suffix)] + suffix
    return value


def table(columns, rows, caption):
    return {"columns": [{"key": key, "label": label} for key, label in columns],
            "rows": [{key: display_cell(row[key]) for key, _label in columns} for row in rows], "caption": caption}


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
                                  "detail": event_text(e, state['decisions'])['detail']} for e in state["journal"]], stamp),
        "paper.reviews": table([("trade_id", "Trade"), ("analysis", "Review"), ("next_action", "Next action")],
                                state["reviews"][-200:], "Reviews of closed trades"),
    }
    if 'strategy' in state:
        strategy = state['strategy']
        rows = [{'setting': 'Status', 'approved': strategy['phase'], 'proposed': ''},
                {'setting': 'Paper account', 'approved': strategy['account_no'], 'proposed': ''}]
        active, proposal = strategy['active'], strategy['proposal']
        pending = proposal if proposal and (not active or proposal['revision'] != active['revision']) else None
        rows.append({'setting': 'Revision (short)', 'approved': active['revision'][:12] if active else '',
                     'proposed': pending['revision'][:12] if pending else ''})
        for key, label in [('research_root', 'Research'), ('symbols', 'Symbols'),
                           ('max_order_usd', 'Order limit / USD'),
                           ('max_portfolio_usd', 'Cost limit / USD'),
                           ('max_trade_risk_usd', 'Price risk / USD'),
                           ('quote_max_age_seconds', 'Quote age / sec'),
                           ('max_price_deviation_bps', 'Deviation / bps')]:
            values = {'setting': label}
            for name, snapshot in [('approved', active), ('proposed', pending)]:
                value = snapshot['settings'][key] if snapshot else ''
                if key == 'research_root' and len(value) > 32:
                    value = value[:8] + '...' + value[-21:]
                values[name] = ', '.join(value) if isinstance(value, list) else value
            rows.append(values)
        result = {'paper.strategy': table([('setting', 'Setting'), ('approved', 'Approved'),
                                            ('proposed', 'Awaiting approval')], rows,
                                          'Only approved settings authorize trading.'),
                  **result}
        result.update(view_payloads(state, result))
    return result
