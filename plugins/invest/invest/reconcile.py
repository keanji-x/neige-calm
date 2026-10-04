"""Validate whole broker observations before atomically adopting executions, per symbol.

Generalized from `plugins/paper-trading/paper_trading/allocation_reconcile.py`: orders are the legs of
decisions, and holdings are checked for every symbol, not SPY alone.
"""
import json
from decimal import Decimal

from .config import broker_money, integer, timestamp
from .ledger import encoded
from .symbols import from_sdk

BROKER_TERMINAL = {"Filled", "Canceled", "Rejected", "Expired", "PartialWithdrawal"}
BROKER_ACTIVE = {"NotReported", "New", "WaitToNew", "PartialFilled", "WaitToReplace",
                 "PendingReplace", "Replaced", "WaitToCancel", "PendingCancel"}
RESOLVED = ('settled', 'canceled', 'rejected', 'expired')


def validate_snapshot(raw, config, now):
    identity = raw['identity']
    if identity.get('account_channel') != 'lb_papertrading' or identity.get('account_no') != config.account_no:
        raise ValueError('paper-account identity check failed')
    cash = broker_money(raw['cash_usd'], zero=True)
    available = broker_money(raw['available_cash_usd'], zero=True)
    if available > cash:
        raise ValueError('broker availability exceeds owned cash')
    if not isinstance(raw['quotes'], dict) or not isinstance(raw['positions'], dict):
        raise ValueError('broker positions and quotes are required')
    quotes = {}
    for key, quote in raw['quotes'].items():
        quote_at = timestamp(quote['at'])
        if quote_at > now or not isinstance(quote['status'], str):
            raise ValueError(f'non-future quote with a status required for {key}')
        quotes[from_sdk(key)] = {'price': str(broker_money(quote['price'])), 'at': quote_at.isoformat(),
                                 'status': quote['status']}
    positions, equity = {}, cash
    for key, held in raw['positions'].items():
        symbol = from_sdk(key)
        shares = integer(held['shares'], zero=True)
        available_shares = integer(held['available_shares'], zero=True)
        if available_shares > shares:
            raise ValueError(f'broker availability exceeds owned shares of {symbol}')
        if not shares:
            continue
        if symbol not in quotes:
            raise ValueError(f'no quote for held {symbol}; the portfolio cannot be valued')
        price = Decimal(quotes[symbol]['price'])
        positions[symbol] = {'shares': shares, 'available_shares': available_shares,
                             'price': str(price), 'value_usd': str(shares * price)}
        equity += shares * price
    if type(raw['market_open']) is not bool:
        raise ValueError('broker market session is required')
    return {'at': now.isoformat(), 'cash_usd': str(cash), 'available_cash_usd': str(available),
            'equity_usd': str(equity), 'market_open': raw['market_open'],
            'positions': dict(sorted(positions.items())), 'quotes': dict(sorted(quotes.items()))}


def broker_orders(raw):
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
    return details


def owned_orders(ledger, db, details):
    """Each leg matched to its broker order: by recorded identity, else by its unique remark."""
    owned, archived = {}, {}
    for leg in ledger.orders(db):
        request, order_id = leg['request'], leg['broker_id']
        if not order_id:
            matches = [k for k, v in details.items() if v['remark'] == request['remark']]
            if len(matches) > 1:
                raise ValueError('multiple broker orders match one leg')
            if not matches:
                continue  # absence does not prove that an uncertain submission failed
            order_id = matches[0]
        if order_id not in details:
            # A resolved order outside the history window keeps its reconciled, persisted fills.
            if leg['state'] in RESOLVED:
                archived[order_id] = leg
                continue
            raise ValueError('known leg order missing from broker history')
        order = details[order_id]
        if any(order[k] != request[k] for k in ('symbol', 'side', 'order_type', 'remark', 'time_in_force', 'outside_rth')):
            raise ValueError('broker market order does not match its leg')
        if integer(order['quantity']) != request['quantity']:
            raise ValueError('broker market-order quantity mismatch')
        if leg['state'] in RESOLVED and order['status'] in BROKER_ACTIVE:
            raise ValueError('terminal order regressed to active')
        if order_id in owned:
            raise ValueError('broker order belongs to multiple legs')
        owned[order_id] = leg
    return owned, archived


def adopt_fills(ledger, db, raw, owned):
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
        if fill['order_id'] not in owned:
            continue
        if fill['symbol'] != owned[fill['order_id']]['request']['symbol']:
            raise ValueError('leg execution has another symbol')
        normalized['symbol'] = from_sdk(fill['symbol'])
        old = db.execute('SELECT body FROM fills WHERE id=?', (key,)).fetchone()
        if old:
            previous = json.loads(old[0]); previous['price'] = broker_money(previous['price'])
            if previous != normalized | {'price': comparable['price']}:
                raise ValueError('conflicting persisted execution identity')
            continue
        db.execute('INSERT INTO fills VALUES (?,?)', (key, encoded(normalized)))
        ledger.event(db, 'leg_fill', normalized)


def reconcile(db, ledger, raw, snapshot, opening):
    """Adopt owned executions and leg states; refuse unowned active orders and unexplained holdings."""
    details = broker_orders(raw)
    owned, archived = owned_orders(ledger, db, details)
    if any(k not in owned and v['status'] in BROKER_ACTIVE for k, v in details.items()):
        raise ValueError('unowned active broker order; execution blocked')
    adopt_fills(ledger, db, raw, owned)
    fills = ledger.fills(db)
    for order_id, leg in owned.items():
        order, quantity = details[order_id], leg['request']['quantity']
        total = sum(f['quantity'] for f in fills if f['order_id'] == order_id)
        if total != integer(order['executed_quantity'], zero=True) or total > quantity:
            raise ValueError('order/execution totals disagree; retry reconciliation')
        if order['status'] == 'Filled' and total != quantity:
            raise ValueError('filled order has incomplete executions')
        if order['status'] in ('PartialFilled', 'PartialWithdrawal') and not 0 < total < quantity:
            raise ValueError('partial execution totals disagree')
        state = {'Filled': 'settled', 'Canceled': 'canceled', 'PartialWithdrawal': 'canceled',
                 'Rejected': 'rejected', 'Expired': 'expired'}.get(order['status'], 'working')
        ledger.order(db, leg['id'], state, broker_id=order_id, broker_status=order['status'])
    # Every symbol's holding is its opening position plus the signed fills of its own legs.
    sides = {order_id: leg['request']['side'] for order_id, leg in (owned | archived).items()}
    expected = dict(opening)
    for fill in fills:
        sign = 1 if sides.get(fill['order_id']) == 'Buy' else -1 if sides.get(fill['order_id']) == 'Sell' else 0
        if not sign:
            raise ValueError('persisted execution has no leg in this observation')
        expected[fill['symbol']] = expected.get(fill['symbol'], 0) + sign * fill['quantity']
    actual = {s: p['shares'] for s, p in snapshot['positions'].items()}
    if {s: n for s, n in expected.items() if n} != actual or any(n < 0 for n in expected.values()):
        raise ValueError('holdings disagree with owned executions; external activity blocked')
