"""Execution legs of the requested decision, from a fresh reconciled snapshot.

One order per symbol beyond the drift band, at most one leg in flight, sells before buys, and buys
funded by settled cash only. Intent is committed before the broker write and an uncertain leg is never
resubmitted (the paper plugin's invariants, per symbol).
"""
from decimal import Decimal, ROUND_FLOOR

from .broker import OrderNotSubmitted
from .config import money, timestamp
from .ledger import digest, encoded
from .symbols import to_sdk

FINAL = frozenset(('done', 'noop', 'expired'))
IN_FLIGHT = frozenset(('submitting', 'working', 'unknown'))
PRICE_RESERVE = Decimal('1.01')  # absorbs modest market-order price movement; not a price guarantee


def floor(value):
    return int(value.to_integral_value(rounding=ROUND_FLOOR))


def leg(config, snapshot, decision, symbol, now):
    """`symbol`'s next order: None inside the band, else `{side, request}` or `{side, wait}`."""
    target = decision['body']['weights'].get(symbol, 0)
    held = snapshot['positions'].get(symbol)
    shares = held['shares'] if held else 0
    quote = snapshot['quotes'].get(symbol)
    if quote is None:
        return {'side': 'Buy', 'wait': 'no broker quote'} if target else None
    try:
        price, equity = money(quote['price']), Decimal(snapshot['equity_usd'])
        if equity <= 0:
            raise ValueError('portfolio has no positive equity')
    except ValueError as error:
        return {'side': 'Sell' if shares else 'Buy', 'wait': str(error)}
    if abs(shares * price / equity * 10000 - target) <= config.drift_bps:
        return None
    delta = floor(equity * target / 10000 / price) - shares
    if not delta:
        return None
    side = 'Buy' if delta > 0 else 'Sell'
    quantity = min(abs(delta), floor(equity * config.max_order_bps / 10000 / (price * PRICE_RESERVE)))
    if not quantity:
        return None  # one order step buys less than one share
    if quote['status'] != 'Normal':
        return {'side': side, 'wait': f"trading status is {quote['status']}"}
    if not 0 <= (now - timestamp(quote['at'])).total_seconds() <= config.quote_max_age_seconds:
        return {'side': side, 'wait': 'quote is stale or future-dated'}
    if side == 'Sell':
        if quantity > held['available_shares']:
            return {'side': side, 'wait': 'shares are not available for sale'}
    else:
        available = min(money(snapshot['available_cash_usd'], zero=True), money(snapshot['cash_usd'], zero=True))
        budget = max(Decimal(0), available - equity * config.cash_buffer_bps / 10000)
        quantity = min(quantity, floor(budget / (price * PRICE_RESERVE)))
        if not quantity:
            return {'side': side, 'wait': 'insufficient settled cash for one share'}
    identity = digest({'account': config.account_no, 'decision': decision['id'], 'symbol': symbol})
    return {'side': side, 'request': {
        'symbol': to_sdk(symbol), 'side': side, 'quantity': quantity, 'order_type': 'MO',
        'time_in_force': 'Day', 'outside_rth': 'RTH_ONLY', 'basis_shares': shares,
        'not_after': decision['body']['valid_until'], 'client_request_id': identity,
        'remark': 'nc-inv-' + identity[:32]}}


def finish(ledger, db, decision, orders, deadline):
    """Resolve a decision with no leg in flight: `done` once any leg was sent, else `noop`, or
    `expired` when its validity ended first. A leg still waiting at the deadline ends without an order."""
    if not orders:
        ledger.decide(db, decision['id'], 'expired' if deadline else 'noop')
        return
    waited = decision['error'] if deadline and decision['state'] == 'working' else None
    ledger.decide(db, decision['id'], 'done', f'valid_until reached; never sent: {waited}' if waited else None)


def expire(ledger, db, now):
    """Local and broker-independent: an unexecuted decision must end at its deadline even while the
    broker is unreadable, or it would block every replacement."""
    for decision in ledger.decisions(db):
        if decision['state'] in FINAL or timestamp(decision['body']['valid_until']) > now:
            continue
        orders = ledger.orders(db, decision['id'])
        if not any(o['state'] in IN_FLIGHT for o in orders):
            finish(ledger, db, decision, orders, deadline=True)


def advance(ledger, db, config, broker, snapshot, clock):
    """Send at most one leg of the requested decision."""
    decision = next((d for d in ledger.decisions(db) if d['state'] in ('requested', 'working')), None)
    if decision is None:
        return
    key, orders = decision['id'], ledger.orders(db, decision['id'])
    if any(o['state'] in IN_FLIGHT for o in orders):
        return
    if timestamp(decision['body']['valid_until']) <= clock():
        finish(ledger, db, decision, orders, deadline=True)
        return
    if not snapshot['market_open']:
        ledger.decide(db, key, decision['state'], 'waiting for the regular session')
        return
    sent = {o['symbol'] for o in orders}
    now = clock()
    pending = [(symbol, step) for symbol in sorted((set(decision['body']['weights']) | set(snapshot['positions'])) - sent)
               if (step := leg(config, snapshot, decision, symbol, now)) is not None]
    for side in ('Sell', 'Buy'):
        ready = [(symbol, step['request']) for symbol, step in pending if step['side'] == side and 'request' in step]
        if ready:
            submit(ledger, db, broker, decision, *ready[0], clock)
            return
        waiting = [f"{symbol}: {step['wait']}" for symbol, step in pending if step['side'] == side]
        if waiting:
            ledger.decide(db, key, decision['state'], '; '.join(waiting))
            return
    finish(ledger, db, decision, orders, deadline=False)


def submit(ledger, db, broker, decision, symbol, request, clock):
    key = decision['id']
    # Sizing saw the decision valid; the deadline may have passed since.
    if timestamp(decision['body']['valid_until']) <= clock():
        finish(ledger, db, decision, ledger.orders(db, key), deadline=True)
        return
    leg_id = request['remark']
    db.execute('INSERT INTO orders(id,decision_id,symbol,request,state) VALUES (?,?,?,?,?)',
               (leg_id, key, symbol, encoded(request), 'submitting'))
    ledger.decide(db, key, 'working')
    ledger.event(db, 'leg_submitting', {'decision_id': key, 'symbol': symbol, 'request': request})
    # Commit BEFORE the broker write while retaining the cross-process lock.
    db.commit()
    try:
        broker_id = broker.submit(request)
        ledger.order(db, leg_id, 'working', broker_id=broker_id)
    except OrderNotSubmitted:
        ledger.order(db, leg_id, 'rejected', error='SDK preflight refused before sending an order')
    except Exception:
        ledger.order(db, leg_id, 'unknown', error='Submission outcome unknown; reconcile, never resubmit')
