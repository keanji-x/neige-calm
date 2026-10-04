"""Planner-owned targets, Worker execution, and durable SPY/cash allocation state."""
from datetime import datetime, timedelta, timezone
from decimal import Decimal, ROUND_FLOOR
from pathlib import Path
import json
import re
from zoneinfo import ZoneInfo

from .config import broker_money, exact, identifier, integer, money, timestamp
from .ledger import Ledger, digest, encoded
from .allocation_reconcile import reconcile, validate_snapshot
from .allocation_broker import OrderNotSubmitted


# Decision IDs also name the Worker task key `spy-exec-<id>` (`^[a-z0-9][a-z0-9._-]{0,63}$`).
DECISION_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,54}')
TOOLS = frozenset(('spy.plan', 'spy.execute', 'spy.status', 'spy.refresh'))
FINAL = frozenset(('settled', 'noop', 'rejected', 'canceled', 'expired'))
NEW_YORK = ZoneInfo('America/New_York')
HISTORY = 260  # about one year of trading-day valuation samples in the overview projection


def valuation_date(quote_at):
    """The America/New_York calendar date that owns a valuation sample."""
    return timestamp(quote_at).astimezone(NEW_YORK).date().isoformat()


class Allocation:
    def __init__(self, root, account, broker, clock=None):
        self.account, self.broker = account, broker
        self.clock = clock or (lambda: datetime.now(timezone.utc))
        root = Path(root)
        if (root / 'ledger.sqlite3').exists() or (root / 'strategy.sqlite3').exists():
            raise ValueError('SPY profile cannot reuse supervised portfolio data; resolve its orders first')
        self.ledger = Ledger(root / 'spy-cash', account)
        with self.ledger.session() as db:
            db.execute('CREATE TABLE IF NOT EXISTS order_requests (id TEXT PRIMARY KEY, body TEXT NOT NULL)')
            db.execute('CREATE TABLE IF NOT EXISTS valuations (date TEXT PRIMARY KEY, body TEXT NOT NULL)')
            binding = {'profile': 'spy_cash', 'oauth_client_id': account.oauth_client_id,
                       'broker_home': account.broker_home}
            old = self.ledger.get_meta(db, 'execution_binding')
            if old is not None and old != binding:
                raise ValueError('SPY execution binding cannot change')
            self.ledger.set_meta(db, 'execution_binding', binding)

    def call(self, track, name, args, caller):
        if track != self.account.owner_track_id:
            raise ValueError('this Track does not own the SPY portfolio')
        if name not in TOOLS:
            raise ValueError('tool unavailable in SPY profile')
        role = caller.get('role') if isinstance(caller, dict) else None
        if role not in ('planner', 'worker') or not all(isinstance(caller.get(k), str) and caller[k]
                                                        for k in ('card_id', 'session_id')):
            raise ValueError('host-provided agent identity required')
        if name == 'spy.plan' and role != 'planner':
            raise ValueError('SPY target requires Planner identity')
        if name == 'spy.execute' and role != 'worker':
            raise ValueError('SPY execution requires Worker identity')
        with self.ledger.session() as db:
            if name == 'spy.plan':
                self.plan(db, args, caller)
            elif name == 'spy.execute':
                self.request(db, args, caller)
            else:
                # spy.refresh only wakes the background loop, which owns every broker read.
                exact(args, set())
            return self.status(db)

    def plan(self, db, args, caller):
        exact(args, {'decision_id', 'target_spy_bps', 'rationale', 'source_refs', 'valid_until'})
        key = args['decision_id']
        if not isinstance(key, str) or not DECISION_ID.fullmatch(key):
            raise ValueError('decision_id must be 1-55 lowercase letters, digits or hyphens, starting with a letter or digit')
        old = db.execute('SELECT body FROM decisions WHERE id=?', (key,)).fetchone()
        if old is not None:
            if old[0] != encoded(args):
                raise ValueError('decision ID already names a different target')
            return
        ratio = args['target_spy_bps']
        if type(ratio) is not int or not 0 <= ratio <= 10000:
            raise ValueError('target_spy_bps must be an integer between 0 and 10000')
        if not isinstance(args['rationale'], str) or not 10 <= len(args['rationale']) <= 6000:
            raise ValueError('rationale must contain 10-6000 characters')
        refs = args['source_refs']
        if not isinstance(refs, list) or not 1 <= len(refs) <= 20 or any(
                not isinstance(r, str) or not r.startswith('neige://source/') or len(r) > 512 for r in refs):
            raise ValueError('1-20 captured Neige source references required')
        now = self.clock()
        if not now < timestamp(args['valid_until']) <= now + timedelta(hours=24):
            raise ValueError('decision expires within the next 24 hours')
        if any(d['state'] not in FINAL for d in self.ledger.decisions(db)):
            raise ValueError('resolve the current allocation before creating another target')
        db.execute('INSERT INTO decisions(id,body,state,created_at) VALUES (?,?,?,?)',
                   (key, encoded(args), 'queued', now.isoformat()))
        self.ledger.event(db, 'allocation_planned', {'decision_id': key, 'caller': caller, **args})

    def refresh(self, db):
        decisions = self.ledger.decisions(db)
        # Only unresolved orders need history; settled ones keep their persisted fills.
        since = min((d['created_at'] for d in decisions
                     if d['state'] in ('submitting', 'unknown', 'working')), default=None)
        raw = self.broker.snapshot(since)
        snapshot = validate_snapshot(raw, self.account, self.clock())
        opening = self.opening_shares(db)
        reconcile(db, self.ledger, raw, snapshot, opening)
        # The first successful reconciliation pins the acknowledged opening holding.
        self.ledger.set_meta(db, 'opening_shares', opening)
        self.ledger.set_meta(db, 'snapshot', snapshot)
        self.ledger.set_meta(db, 'error', None)
        # One reconciled valuation per New York quote date; the latest observation of that date wins.
        sample = {'date': valuation_date(snapshot['quote_at']), 'at': snapshot['at'],
                  'equity_usd': snapshot['equity_usd'], 'cash_usd': snapshot['cash_usd'],
                  'shares': snapshot['shares'], 'price': snapshot['price']}
        db.execute('INSERT INTO valuations VALUES (?,?) ON CONFLICT(date) DO UPDATE SET body=excluded.body',
                   (sample['date'], encoded(sample)))
        return snapshot

    def opening_shares(self, db):
        """The operator-acknowledged SPY holding this ledger started from; immutable once reconciled."""
        pinned = self.ledger.get_meta(db, 'opening_shares')
        if pinned is None and self.ledger.get_meta(db, 'snapshot') is not None:
            pinned = 0  # reconciled before the field existed, so it started from no shares
        if pinned is not None and pinned != self.account.opening_shares:
            raise ValueError(f'opening_shares cannot change on this ledger (reconciled with {pinned})')
        return self.account.opening_shares

    def size(self, snapshot, plan):
        price = money(snapshot['price'])
        shares = snapshot['shares']
        equity = money(snapshot['cash_usd'], zero=True) + shares * price
        if equity <= 0:
            raise ValueError('portfolio has no positive cash/equity')
        target_bps = min(plan['target_spy_bps'], 10000 - self.account.cash_buffer_bps)
        current_bps = Decimal(shares) * price / equity * 10000
        if abs(current_bps - target_bps) <= self.account.drift_bps:
            return None
        target = int((equity * target_bps / 10000 / price).to_integral_value(rounding=ROUND_FLOOR))
        delta = target - shares
        if not delta:
            return None
        side = 'Buy' if delta > 0 else 'Sell'
        step_budget = equity * self.account.max_order_bps / 10000
        quantity = min(abs(delta), int((step_budget / (price * Decimal('1.01'))).to_integral_value(rounding=ROUND_FLOOR)))
        if not quantity:
            return None
        if side == 'Buy':
            available = min(money(snapshot['available_cash_usd'], zero=True), money(snapshot['cash_usd'], zero=True))
            budget = max(Decimal(0), available - equity * self.account.cash_buffer_bps / 10000)
            # Buffer absorbs modest market-order price movement; it is not a price guarantee.
            quantity = min(quantity, int((budget / (price * Decimal('1.01'))).to_integral_value(rounding=ROUND_FLOOR)))
        elif quantity > snapshot['available_shares']:
            raise ValueError('SPY shares are not available for sale')
        if not quantity:
            raise ValueError('insufficient settled cash for one SPY share')
        return {'symbol': 'SPY.US', 'side': side, 'quantity': quantity, 'order_type': 'MO',
                'time_in_force': 'Day', 'outside_rth': 'RTH_ONLY', 'basis_shares': shares,
                'not_after': plan['valid_until'],
                'client_request_id': digest({'account': self.account.account_no, 'plan': plan}),
                'remark': 'nc-spy-' + digest({'account': self.account.account_no, 'plan': plan})[:32]}

    def request(self, db, args, caller):
        """Record the Worker's execution request; the background loop performs it."""
        exact(args, {'decision_id'})
        key = identifier(args['decision_id'])
        decision = self.ledger.decision(db, key)
        if decision['state'] == 'requested':
            return
        if decision['state'] != 'queued':
            raise ValueError(f"SPY decision is {decision['state']}, not awaiting execution; read spy.status")
        if timestamp(decision['body']['valid_until']) <= self.clock():
            raise ValueError('SPY decision expired; the Planner must save a new target')
        self.ledger.change(db, key, 'requested')
        self.ledger.event(db, 'allocation_execution_requested', {'decision_id': key, 'caller': caller})

    def advance(self, db, snapshot):
        """Submit at most one order, for the requested decision, from a fresh reconciled snapshot."""
        decision = next((d for d in self.ledger.decisions(db) if d['state'] == 'requested'), None)
        if decision is None:
            return
        key = decision['id']
        try:
            if not snapshot['market_open']:
                raise ValueError('waiting for the regular session')
            age = (self.clock() - timestamp(snapshot['quote_at'])).total_seconds()
            if not 0 <= age <= self.account.quote_max_age_seconds:
                raise ValueError('SPY quote is stale or future-dated')
            request = self.size(snapshot, decision['body'])
        except ValueError as error:
            # No broker write yet: keep the request and retry each poll until the decision expires.
            self.ledger.change(db, key, 'requested', error=str(error))
            return
        # Reconciliation saw the decision valid; the deadline may have passed since.
        if timestamp(decision['body']['valid_until']) <= self.clock():
            self.ledger.change(db, key, 'expired')
            return
        if request is None:
            self.ledger.change(db, key, 'noop')
            self.ledger.event(db, 'allocation_noop', {'decision_id': key})
            return
        db.execute('INSERT INTO order_requests VALUES (?,?)', (key, encoded(request)))
        self.ledger.change(db, key, 'submitting')
        self.ledger.event(db, 'allocation_submitting', {'decision_id': key, 'request': request,
                                                      'snapshot': snapshot})
        # Commit BEFORE the broker write while retaining the cross-process lock.
        db.commit()
        try:
            order_id = self.broker.submit(request)
            self.ledger.change(db, key, 'working', broker_id=order_id)
            self.ledger.event(db, 'allocation_submitted', {'decision_id': key, 'order_id': order_id})
        except OrderNotSubmitted:
            self.ledger.change(db, key, 'rejected', error='SDK preflight refused before sending an order')
            self.ledger.event(db, 'allocation_not_submitted', {'decision_id': key})
        except Exception:
            self.ledger.change(db, key, 'unknown', error='Submission outcome unknown; reconcile, never resubmit')
            self.ledger.event(db, 'allocation_unknown', {'decision_id': key})

    def status(self, db):
        snapshot = self.ledger.get_meta(db, 'snapshot')
        decisions = self.ledger.decisions(db)
        requests = {r['id']: json.loads(r['body']) for r in db.execute('SELECT * FROM order_requests')}
        fills = sorted(self.ledger.fills(db), key=lambda f: timestamp(f['time']))
        for d in decisions:
            d['order_request'] = requests.get(d['id'])
            # Complete per-decision totals: the returned fills list below is bounded.
            d['filled_quantity'] = sum(f['quantity'] for f in fills if d['broker_id'] and f['order_id'] == d['broker_id'])
        return {'profile': 'spy_cash', 'symbol': 'SPY.US', 'snapshot': snapshot,
                'policy': {'max_order_bps': self.account.max_order_bps,
                           'cash_buffer_bps': self.account.cash_buffer_bps},
                'error': self.ledger.get_meta(db, 'error'), 'decisions': decisions[-200:],
                'fills': fills[-200:], 'journal': [dict(r) | {'body': json.loads(r['body'])}
                    for r in db.execute('SELECT * FROM journal ORDER BY seq DESC LIMIT 200')]}

    def projection(self, db):
        """Overlay input: status plus the valuation history, which no agent tool returns."""
        return self.status(db) | {'valuations': [json.loads(r[0]) for r in db.execute(
            'SELECT body FROM valuations ORDER BY date DESC LIMIT ?', (HISTORY,))][::-1]}

    def process_once(self):
        # Local and broker-independent: an unsubmitted decision must expire even while the
        # broker is unreadable, or it would block every replacement target.
        with self.ledger.session() as db:
            now = self.clock()
            for d in self.ledger.decisions(db):
                if d['state'] in ('queued', 'requested') and timestamp(d['body']['valid_until']) <= now:
                    self.ledger.change(db, d['id'], 'expired')
        try:
            with self.ledger.session() as db:
                self.advance(db, self.refresh(db))
        except Exception as error:
            # The whole observation rolled back; nothing was submitted from it.
            reason = str(error) if isinstance(error, ValueError) else 'broker unavailable'
            with self.ledger.session() as db:
                self.ledger.set_meta(db, 'error', f'Broker reconciliation failed ({reason}); previous snapshot retained')
        with self.ledger.session() as db:
            return [(self.account.owner_track_id, self.projection(db))]
