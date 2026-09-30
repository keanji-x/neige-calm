"""Planner-owned targets, Worker execution, and durable SPY/cash allocation state."""
from datetime import datetime, timedelta, timezone
from decimal import Decimal, ROUND_FLOOR
from pathlib import Path
import json

from .config import broker_money, exact, identifier, integer, money, timestamp
from .ledger import Ledger, digest, encoded
from .allocation_reconcile import reconcile, validate_snapshot
from .allocation_broker import OrderNotSubmitted


TOOLS = frozenset(('spy.plan', 'spy.execute', 'spy.status', 'spy.refresh'))
FINAL = frozenset(('settled', 'noop', 'rejected', 'canceled', 'expired'))


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
        if name == 'spy.execute' and (role != 'worker' or caller.get('delegated_tool') is not True):
            raise ValueError('SPY execution requires delegated Worker identity')
        if name == 'spy.plan':
            with self.ledger.session() as db:
                self.plan(db, args, caller)
        elif name == 'spy.execute':
            self.execute(args, caller)
        elif name == 'spy.refresh':
            exact(args, set())
            self.refresh()
        else:
            exact(args, set())
        with self.ledger.session() as db:
            return self.status(db)

    def plan(self, db, args, caller):
        exact(args, {'decision_id', 'target_spy_bps', 'rationale', 'source_refs', 'valid_until'})
        key = identifier(args['decision_id'])
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

    def refresh(self, db=None):
        if db is None:
            with self.ledger.session() as opened:
                return self.refresh(opened)
        decisions = self.ledger.decisions(db)
        since = min((d['created_at'] for d in decisions if d['broker_id'] or
                     d['state'] in ('submitting', 'unknown', 'working')), default=None)
        raw = self.broker.snapshot(since)
        snapshot = validate_snapshot(raw, self.account, self.clock())
        reconcile(db, self.ledger, raw, snapshot)
        self.ledger.set_meta(db, 'snapshot', snapshot)
        self.ledger.set_meta(db, 'error', None)
        return snapshot

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
                'client_request_id': digest({'account': self.account.account_no, 'plan': plan}),
                'remark': 'nc-spy-' + digest({'account': self.account.account_no, 'plan': plan})[:32]}

    def execute(self, args, caller):
        exact(args, {'decision_id'})
        key = identifier(args['decision_id'])
        with self.ledger.session() as db:
            decision = self.ledger.decision(db, key)
            if decision is None:
                raise ValueError('unknown SPY decision')
            # Once submission might have happened, recovery only reads the broker.
            if decision['state'] != 'queued':
                self.refresh(db)
                return
            snapshot = self.refresh(db)
            if timestamp(decision['body']['valid_until']) <= self.clock():
                self.ledger.change(db, key, 'expired')
                return
            if not snapshot['market_open']:
                self.ledger.event(db, 'allocation_waiting', {'decision_id': key, 'reason': 'regular session required'})
                return
            age = (self.clock() - timestamp(snapshot['quote_at'])).total_seconds()
            if not 0 <= age <= self.account.quote_max_age_seconds:
                raise ValueError('SPY quote is stale or future-dated')
            request = self.size(snapshot, decision['body'])
            if request is None:
                self.ledger.change(db, key, 'noop')
                self.ledger.event(db, 'allocation_noop', {'decision_id': key})
                return
            db.execute('INSERT INTO order_requests VALUES (?,?)', (key, encoded(request)))
            self.ledger.change(db, key, 'submitting')
            self.ledger.event(db, 'allocation_submitting', {'decision_id': key, 'request': request,
                                                          'caller': caller, 'snapshot': snapshot})
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
        for d in decisions:
            d['order_request'] = requests.get(d['id'])
        return {'profile': 'spy_cash', 'symbol': 'SPY.US', 'snapshot': snapshot,
                'policy': {'max_order_bps': self.account.max_order_bps,
                           'cash_buffer_bps': self.account.cash_buffer_bps},
                'error': self.ledger.get_meta(db, 'error'), 'decisions': decisions[-200:],
                'fills': self.ledger.fills(db)[-200:], 'journal': [dict(r) | {'body': json.loads(r['body'])}
                    for r in db.execute('SELECT * FROM journal ORDER BY seq DESC LIMIT 200')]}

    def process_once(self):
        try:
            self.refresh()
        except Exception:
            with self.ledger.session() as db:
                self.ledger.set_meta(db, 'error', 'Broker reconciliation failed; previous snapshot retained')
        with self.ledger.session() as db:
            return [(self.account.owner_track_id, self.status(db))]
