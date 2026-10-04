"""Validate whole broker observations before atomically adopting executions."""
import json
from decimal import Decimal

from .config import broker_money, integer, timestamp
from .ledger import encoded

BROKER_TERMINAL = {"Filled", "Canceled", "Rejected", "Expired", "PartialWithdrawal"}
BROKER_ACTIVE = {"NotReported", "New", "WaitToNew", "PartialFilled", "WaitToReplace",
                 "PendingReplace", "Replaced", "WaitToCancel", "PendingCancel"}


def validate_snapshot(raw, config, now):
    identity = raw['identity']
    if identity.get('account_channel') != 'lb_papertrading' or identity.get('account_no') != config.account_no:
        raise ValueError('SPY paper-account identity check failed')
    cash = broker_money(raw['cash_usd'], zero=True)
    available = broker_money(raw['available_cash_usd'], zero=True)
    shares = integer(raw['shares'], zero=True)
    available_shares = integer(raw['available_shares'], zero=True)
    if available > cash or available_shares > shares:
        raise ValueError('broker availability exceeds owned cash/shares')
    price = broker_money(raw['quote']['price'])
    quote_at = timestamp(raw['quote']['at'])
    if raw['quote']['status'] != 'Normal' or quote_at > now:
        raise ValueError('normal, non-future SPY quote required')
    if type(raw['market_open']) is not bool:
        raise ValueError('broker market session is required')
    equity = cash + shares * price
    return {'at': now.isoformat(), 'cash_usd': str(cash), 'available_cash_usd': str(available),
            'shares': shares, 'available_shares': available_shares, 'price': str(price),
            'quote_at': quote_at.isoformat(), 'market_open': raw['market_open'],
            'equity_usd': str(equity),
            'actual_spy_bps': str(Decimal(shares) * price / equity * 10000) if equity else '0'}


def reconcile(db, ledger, raw, snapshot, opening_shares):
    decisions = ledger.decisions(db)
    requests = {r['id']: json.loads(r['body']) for r in db.execute('SELECT * FROM order_requests')}
    if not isinstance(raw['orders'], list) or len(raw['orders']) > 500:
        raise ValueError('broker order history exceeds reconciliation budget')
    details = {}
    for order in raw['orders']:
        key = order['order_id']
        if not isinstance(key, str) or not key:
            raise ValueError('broker order identity required')
        if order['status'] not in BROKER_ACTIVE | BROKER_TERMINAL:
            raise ValueError('unknown broker order status')
        if key in details and details[key] != order:
            raise ValueError('conflicting broker order identity')
        details[key] = order
    owned, archived = {}, {}
    for decision in decisions:
        request = requests.get(decision['id'])
        if request is None:
            if decision['broker_id'] or decision['state'] in ('submitting', 'unknown', 'working', 'settled'):
                raise ValueError('submitted decision has no persisted request')
            continue
        order_id = decision['broker_id']
        if not order_id:
            matches = [k for k, v in details.items() if v['remark'] == request['remark']]
            if len(matches) > 1:
                raise ValueError('multiple orders match one allocation')
            if not matches:
                continue  # absence does not prove that an uncertain submission failed
            order_id = matches[0]
        if order_id not in details:
            # A resolved order outside the history window keeps its reconciled, persisted fills.
            if decision['state'] in ('settled', 'canceled', 'rejected', 'expired'):
                archived[order_id] = request
                continue
            raise ValueError('known allocation order missing from broker history')
        order = details[order_id]
        if any(order[k] != request[k] for k in ('symbol', 'side', 'order_type', 'remark', 'time_in_force', 'outside_rth')):
            raise ValueError('broker market order does not match allocation request')
        if integer(order['quantity']) != request['quantity']:
            raise ValueError('broker market-order quantity mismatch')
        if decision['state'] in ('settled', 'canceled', 'rejected', 'expired') and order['status'] in BROKER_ACTIVE:
            raise ValueError('terminal order regressed to active')
        if order_id in owned:
            raise ValueError('broker order belongs to multiple decisions')
        owned[order_id] = decision | {'request': request}
    if any(k not in owned and v['status'] in BROKER_ACTIVE for k, v in details.items()):
        raise ValueError('unowned active broker order; SPY execution blocked')
    if not isinstance(raw['fills'], list) or len(raw['fills']) > 5000:
        raise ValueError('broker execution history exceeds reconciliation budget')
    seen = {}
    for fill in raw['fills']:
        key = fill['trade_id']
        if not isinstance(key, str) or not key:
            raise ValueError('broker execution identity required')
        normalized = {'trade_id': key, 'order_id': fill['order_id'], 'symbol': fill['symbol'],
                      'quantity': integer(fill['quantity']), 'price': str(broker_money(fill['price'])),
                      'time': timestamp(fill['time']).isoformat()}
        comparable = normalized | {'price': broker_money(normalized['price'])}
        if key in seen and seen[key] != comparable:
            raise ValueError('conflicting broker execution identity')
        seen[key] = comparable
        old = db.execute('SELECT body FROM fills WHERE id=?', (key,)).fetchone()
        if old:
            previous = json.loads(old[0]); previous['price'] = broker_money(previous['price'])
            if previous != comparable:
                raise ValueError('conflicting persisted execution identity')
        if fill['order_id'] not in owned:
            continue
        if fill['symbol'] != 'SPY.US':
            raise ValueError('allocation execution has another symbol')
        if old is None:
            db.execute('INSERT INTO fills VALUES (?,?)', (key, encoded(normalized)))
            ledger.event(db, 'allocation_fill', normalized)
    fills = ledger.fills(db)
    expected_shares = opening_shares + sum(f['quantity'] * (1 if archived[f['order_id']]['side'] == 'Buy' else -1)
                          for f in fills if f['order_id'] in archived)
    for order_id, decision in owned.items():
        order = details[order_id]
        total = sum(f['quantity'] for f in fills if f['order_id'] == order_id)
        reported = integer(order['executed_quantity'], zero=True)
        if total != reported or total > decision['request']['quantity']:
            raise ValueError('order/execution totals disagree; retry reconciliation')
        if order['status'] == 'Filled' and total != decision['request']['quantity']:
            raise ValueError('filled order has incomplete executions')
        if order['status'] in ('PartialFilled', 'PartialWithdrawal') and not 0 < total < decision['request']['quantity']:
            raise ValueError('partial execution totals disagree')
        expected_shares += total * (1 if decision['request']['side'] == 'Buy' else -1)
        state = {'Filled': 'settled', 'Canceled': 'canceled', 'PartialWithdrawal': 'canceled',
                 'Rejected': 'rejected', 'Expired': 'expired'}.get(order['status'], 'working')
        ledger.change(db, decision['id'], state, broker_id=order_id, broker_status=order['status'])
    if expected_shares < 0 or expected_shares != snapshot['shares']:
        raise ValueError('SPY holdings disagree with owned executions; external activity blocked')
