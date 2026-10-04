"""The invest portfolio Track: Planner decisions, Worker execution requests and the one
reconcile/submit loop that is the only broker writer."""
from datetime import datetime, timedelta, timezone
import json
import re
from zoneinfo import ZoneInfo

from . import instruments
from .config import exact, identifier, timestamp
from .execution import FINAL, IN_FLIGHT, advance, expire
from .ledger import Ledger, encoded
from .reconcile import reconcile, validate_snapshot
from .symbols import canonical, to_sdk

# Decision IDs also name the Worker task key `inv-exec-<id>` (`^[a-z0-9][a-z0-9._-]{0,63}$`).
DECISION_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,54}')
ROLES = {'portfolio_status': ('planner', 'worker'), 'decision_add': ('planner',), 'execution_add': ('worker',)}
TOOLS = frozenset(ROLES)
PORTFOLIO_TOOLS = TOOLS  # every tool of this slice acts on the portfolio Track
NEW_YORK = ZoneInfo('America/New_York')
HISTORY = 260  # about one year of trading-day valuation samples
FILL_LOG = 500  # the fill table's rows; agent responses carry the latest 200


def valuation_date(at):
    """The America/New_York calendar date that owns a valuation sample."""
    return timestamp(at).astimezone(NEW_YORK).date().isoformat()


def parse_weights(raw):
    """`[{symbol, bps}]` → `{VENUE:CODE: bps}`; shape only, state is checked separately."""
    if not isinstance(raw, list) or len(raw) > 255:
        raise ValueError('weights must be a list of at most 255 {symbol, bps} entries')
    weights = {}
    for entry in raw:
        exact(entry, {'symbol', 'bps'})
        symbol = canonical(entry['symbol'])
        if type(entry['bps']) is not int or not 0 <= entry['bps'] <= 10000:
            raise ValueError(f'bps of {symbol} must be an integer between 0 and 10000')
        if symbol in weights:
            raise ValueError(f'{symbol} is weighted more than once')
        weights[symbol] = entry['bps']
    return dict(sorted(weights.items()))


class Portfolio:
    def __init__(self, root, config, broker, clock=None):
        self.config, self.broker = config, broker
        self.clock = clock or (lambda: datetime.now(timezone.utc))
        self.ledger = Ledger(root, config)
        with self.ledger.session() as db:
            # Each acknowledged opening position starts as a pending, held instrument; a changed
            # acknowledgement is refused by reconciliation and admits nothing.
            if self.ledger.get_meta(db, 'opening_positions') in (None, dict(config.opening_positions)):
                for symbol, _shares in config.opening_positions:
                    instruments.admit(self.ledger, db, symbol, self.clock())

    def call(self, track, name, args, caller):
        try:
            if name not in TOOLS:
                raise ValueError(f'unknown tool; the invest tools are {sorted(TOOLS)}')
            if name in PORTFOLIO_TOOLS and track != self.config.portfolio_track_id:
                raise ValueError('only the portfolio Track may call this tool; research Tracks never trade')
            role = caller.get('role') if isinstance(caller, dict) else None
            if role not in ('planner', 'worker') or not all(isinstance(caller.get(k), str) and caller[k]
                                                            for k in ('card_id', 'session_id')):
                raise ValueError('host-provided agent identity required')
            if role not in ROLES[name]:
                raise ValueError(f"requires {' or '.join(r.capitalize() for r in ROLES[name])} identity")
            with self.ledger.session() as db:
                if name == 'decision_add':
                    self.decision_add(db, args, caller)
                elif name == 'execution_add':
                    self.execution_add(db, args, caller)
                else:
                    exact(args, set())
                return self.status(db)
        except ValueError as error:
            raise ValueError(f'{name}: {error}') from None

    def positions(self, db):
        """Symbols with shares: the latest reconciliation, else the acknowledged opening positions."""
        snapshot = self.ledger.get_meta(db, 'snapshot')
        if snapshot is None:
            return {s: n for s, n in self.config.opening_positions}
        return {s: p['shares'] for s, p in snapshot['positions'].items()}

    def decision_add(self, db, args, caller):
        exact(args, {'decision_id', 'weights', 'message', 'source_refs', 'valid_until'})
        key = args['decision_id']
        if not isinstance(key, str) or not DECISION_ID.fullmatch(key):
            raise ValueError('decision_id must be 1-55 lowercase letters, digits or hyphens')
        body = {'weights': parse_weights(args['weights']), 'message': args['message'],
                'source_refs': args['source_refs'], 'valid_until': args['valid_until']}
        old = db.execute('SELECT body FROM decisions WHERE id=?', (key,)).fetchone()
        if old is not None:
            if old[0] != encoded(body):
                raise ValueError('decision_id already names a different decision')
            return
        if not isinstance(body['message'], str) or not 10 <= len(body['message']) <= 6000:
            raise ValueError('message must contain 10-6000 characters')
        refs = body['source_refs']
        if not isinstance(refs, list) or not 1 <= len(refs) <= 20 or any(
                not isinstance(r, str) or not r.startswith('neige://source/') or len(r) > 512 for r in refs):
            raise ValueError('1-20 captured neige://source/ references required')
        now = self.clock()
        if not now < timestamp(body['valid_until']) <= now + timedelta(hours=24):
            raise ValueError('valid_until must fall within the next 24 hours')
        if any(d['state'] not in FINAL for d in self.ledger.decisions(db)):
            raise ValueError('resolve the current decision before adding another')
        self.check_bounds(db, body['weights'])
        db.execute('INSERT INTO decisions(id,body,state,created_at) VALUES (?,?,?,?)',
                   (key, encoded(body), 'queued', now.isoformat()))
        self.ledger.event(db, 'decision_added', {'decision_id': key, 'caller': caller, **body})

    def check_bounds(self, db, weights):
        config, covered = self.config, instruments.counted(db)
        for symbol, bps in weights.items():
            if symbol not in covered:
                raise ValueError(f'{symbol} is not a covered instrument')
            if bps and covered[symbol] != 'live':
                raise ValueError(f'{symbol} is {covered[symbol]}; a weight above 0 requires a live instrument')
            if bps > config.max_weight_bps:
                raise ValueError(f'{symbol} weight {bps} exceeds max_weight_bps {config.max_weight_bps}')
        if sum(weights.values()) > 10000 - config.cash_buffer_bps:
            raise ValueError(f'weights sum to {sum(weights.values())}, above 10000 - cash_buffer_bps '
                             f'({10000 - config.cash_buffer_bps})')
        held_after = {s for s, bps in weights.items() if bps} | set(self.positions(db))
        if len(held_after) > config.max_held:
            raise ValueError(f'{len(held_after)} symbols would be held (weighted or still in a position), '
                             f'above max_held {config.max_held}; sell first, then buy once the sells settle')

    def execution_add(self, db, args, caller):
        """Record the Worker's execution request; the background loop performs it."""
        exact(args, {'decision_id'})
        key = identifier(args['decision_id'])
        decision = self.ledger.decision(db, key)
        if decision['state'] == 'requested':
            return
        if decision['state'] != 'queued':
            raise ValueError(f"decision {key} is {decision['state']}, not awaiting execution; read portfolio_status")
        if timestamp(decision['body']['valid_until']) <= self.clock():
            raise ValueError(f'decision {key} expired; the Planner must add a new decision')
        self.ledger.decide(db, key, 'requested')
        self.ledger.event(db, 'execution_requested', {'decision_id': key, 'caller': caller})

    def opening(self, db):
        """The operator-acknowledged opening positions; immutable once a reconciliation pinned them."""
        configured = dict(self.config.opening_positions)
        pinned = self.ledger.get_meta(db, 'opening_positions')
        if pinned is not None and pinned != configured:
            raise ValueError(f'opening_positions cannot change on this ledger (reconciled with {pinned})')
        return configured

    def refresh(self, db):
        decisions = {d['id']: d for d in self.ledger.decisions(db)}
        # Only unresolved legs need broker history; resolved ones keep their persisted fills.
        since = min((decisions[o['decision_id']]['created_at'] for o in self.ledger.orders(db)
                     if o['state'] in IN_FLIGHT), default=None)
        symbols = [to_sdk(s) for s in sorted(instruments.counted(db))]
        raw = self.broker.snapshot(since, symbols)
        snapshot = validate_snapshot(raw, self.config, self.clock())
        opening = self.opening(db)
        reconcile(db, self.ledger, raw, snapshot, opening)
        # The first successful reconciliation pins the acknowledged opening positions.
        self.ledger.set_meta(db, 'opening_positions', opening)
        self.ledger.set_meta(db, 'snapshot', snapshot)
        self.ledger.set_meta(db, 'error', None)
        instruments.verify(self.ledger, db, snapshot)
        sample = {'date': valuation_date(snapshot['at']), 'at': snapshot['at'],
                  'equity_usd': snapshot['equity_usd'], 'cash_usd': snapshot['cash_usd'],
                  'positions': {s: {'shares': p['shares'], 'price': p['price']}
                                for s, p in snapshot['positions'].items()}}
        db.execute('INSERT INTO valuations VALUES (?,?) ON CONFLICT(date) DO UPDATE SET body=excluded.body',
                   (sample['date'], encoded(sample)))
        return snapshot

    def status(self, db, fills_shown=200):
        decisions = self.ledger.decisions(db)
        legs = self.ledger.orders(db)
        fills = sorted(self.ledger.fills(db), key=lambda f: timestamp(f['time']))
        for d in decisions:
            d['orders'] = [o | {'filled_quantity': sum(f['quantity'] for f in fills
                                                       if o['broker_id'] and f['order_id'] == o['broker_id'])}
                           for o in legs if o['decision_id'] == d['id']]
        targets = decisions[-1]['body']['weights'] if decisions else {}
        held = {s for s, bps in targets.items() if bps} | set(self.positions(db))
        rows = [{'symbol': i['symbol'], 'state': i['state'], 'version': i['version'],
                 'held': i['state'] in instruments.COUNTED and i['symbol'] in held, **json.loads(i['body'])}
                for i in instruments.listed(db)]
        counted = [i for i in rows if i['state'] in instruments.COUNTED]
        return {'snapshot': self.ledger.get_meta(db, 'snapshot'),
                'opening_positions': self.ledger.get_meta(db, 'opening_positions'),
                'instruments': rows, 'targets': targets,
                'limits': {'max_held': self.config.max_held, 'max_watched': self.config.max_watched,
                           'max_weight_bps': self.config.max_weight_bps,
                           'held': sum(i['held'] for i in counted), 'watched': sum(not i['held'] for i in counted)},
                'policy': {'max_order_bps': self.config.max_order_bps, 'cash_buffer_bps': self.config.cash_buffer_bps,
                           'drift_bps': self.config.drift_bps},
                'error': self.ledger.get_meta(db, 'error'), 'decisions': decisions[-200:], 'fills': fills[-fills_shown:],
                'journal': [dict(r) | {'body': json.loads(r['body'])}
                            for r in db.execute('SELECT * FROM journal ORDER BY seq DESC LIMIT 200')]}

    def projection(self, db):
        """Overlay input: status plus the valuation history, which no agent tool returns."""
        return self.status(db, FILL_LOG) | {'valuations': [json.loads(r[0]) for r in db.execute(
            'SELECT body FROM valuations ORDER BY date DESC LIMIT ?', (HISTORY,))][::-1]}

    def process_once(self):
        with self.ledger.session() as db:
            expire(self.ledger, db, self.clock())
        try:
            with self.ledger.session() as db:
                advance(self.ledger, db, self.config, self.broker, self.refresh(db), self.clock)
        except Exception as error:
            # The whole observation rolled back; nothing was submitted from it.
            reason = str(error) if isinstance(error, ValueError) else 'broker unavailable'
            with self.ledger.session() as db:
                self.ledger.set_meta(db, 'error', f'Broker reconciliation failed ({reason}); previous snapshot retained')
        with self.ledger.session() as db:
            return [(self.config.portfolio_track_id, self.projection(db))]
