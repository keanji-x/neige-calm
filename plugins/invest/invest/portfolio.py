"""The invest App's tools and its one reconcile/submit loop, the only broker writer.

The portfolio Track decides, executes, covers instruments and raises theses; a research Track that the
portfolio Track added assesses its own symbol's theses and never trades (#2104 §3.1, §3.6)."""
from datetime import datetime, timedelta, timezone
import json
import re

from . import instruments, research, theses
from .config import captured, exact, identifier, text, timestamp
from .errors import FORBIDDEN, Refused
from .execution import FINAL, IN_FLIGHT, advance, expire
from .ledger import Ledger, encoded
from .reconcile import reconcile, validate_snapshot
from .symbols import canonical, to_sdk

# Decision IDs also name the Worker task key `inv-exec-<id>` (`^[a-z0-9][a-z0-9._-]{0,63}$`).
DECISION_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,54}')
ROLES = {'portfolio_status': ('planner', 'worker'), 'decision_add': ('planner',), 'execution_add': ('worker',),
         'instrument_add': ('planner',), 'instrument_set': ('planner',), 'instrument_rm': ('planner',),
         'thesis_add': ('planner',), 'thesis_rm': ('planner',),
         'instrument_status': ('planner',), 'thesis_set': ('planner',)}
TOOLS = frozenset(ROLES)
RESEARCH_TOOLS = frozenset(('instrument_status', 'thesis_set'))  # attested by provenance (§3.3)
PORTFOLIO_TOOLS = TOOLS - RESEARCH_TOOLS  # only the portfolio Track
VIEWS = frozenset(('portfolio_status', 'instrument_status'))
MESSAGE = 2000  # characters of an audit note
HISTORY = 260  # about one year of trading sessions: one valuation sample per session
FILL_LOG = 500  # the fill table's rows; agent responses carry the latest 200


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
            # Each acknowledged opening position starts as a pending, held instrument, until the first
            # reconciliation pins them; a restart never re-admits a symbol later removed.
            if self.ledger.get_meta(db, 'opening_positions') is None:
                for symbol, _shares in config.opening_positions:
                    instruments.admit(self.ledger, db, symbol, self.clock())

    def call(self, track, name, args, caller):
        """`track` is the host's `_meta["dev.neige/track"]`, `caller` its `dev.neige/caller`."""
        try:
            if name not in TOOLS:
                raise ValueError(f'unknown tool; the invest tools are {sorted(TOOLS)}')
            track = research.context(track)
            if name in PORTFOLIO_TOOLS and track['id'] != self.config.portfolio_track_id:
                raise ValueError('only the portfolio Track may call this tool; research Tracks never trade')
            role = caller.get('role') if isinstance(caller, dict) else None
            if role not in ('planner', 'worker') or not all(isinstance(caller.get(k), str) and caller[k]
                                                            for k in ('card_id', 'session_id')):
                raise ValueError('host-provided agent identity required')
            if role not in ROLES[name]:
                raise ValueError(f"requires {' or '.join(r.capitalize() for r in ROLES[name])} identity")
            with self.ledger.session() as db:
                if name in RESEARCH_TOOLS:
                    return self.research_call(db, track, name, args, caller)
                result = {}
                if name == 'decision_add':
                    self.decision_add(db, args, caller)
                elif name == 'execution_add':
                    self.execution_add(db, args, caller)
                elif name.startswith('instrument_'):
                    result = self.instrument_write(db, name, args, caller)
                elif name == 'thesis_add':
                    theses.add(self.ledger, db, args, self.clock(), caller=caller)
                elif name == 'thesis_rm':
                    theses.remove(self.ledger, db, args, self.clock(), caller=caller)
                else:
                    exact(args, set())
                return self.status(db) | result
        except Refused as error:
            raise Refused(error.code, f'{name}: {error}') from None
        except ValueError as error:
            raise ValueError(f'{name}: {error}') from None

    def instrument_write(self, db, name, args, caller):
        """`instrument_add`, `instrument_set` (renew the issued key) and `instrument_rm`."""
        exact(args, {'symbol', 'message'} | ({'expected_version'} if name != 'instrument_add' else set()))
        symbol, message, now = canonical(args['symbol']), text(args['message'], 'message', MESSAGE), self.clock()
        if name == 'instrument_set':
            return {'track_add': instruments.renew(self.ledger, db, self.config, symbol, args['expected_version'],
                                                   now, message=message, caller=caller)}
        _targets, held = self.held(db)
        if name == 'instrument_rm':
            instruments.current(db, symbol, args['expected_version'])
            if symbol in held:
                raise ValueError(f'{symbol} is held (weighted above 0 by the latest decision, or in a position); '
                                 'sell it to 0 and let the sale settle first')
            instruments.drop(self.ledger, db, symbol, f'removed: {message}', caller=caller)
            theses.retire_open(self.ledger, db, symbol, now, message, caller=caller)
            return {}
        row = instruments.get(db, symbol)
        if row is not None and row['state'] in instruments.COUNTED:
            return {}  # already covered
        watched = sum(s not in held for s in instruments.counted(db))
        if watched >= self.config.max_watched:
            raise ValueError(f'{watched} symbols are watched (covered, not held), at max_watched '
                             f'{self.config.max_watched}; remove one with instrument_rm first')
        instruments.admit(self.ledger, db, symbol, now, message=message, caller=caller)
        return {}

    def research_call(self, db, track, name, args, caller):
        """An attested research call: stamp the lease, act on the caller's own symbol, return its view."""
        row = research.attest(db, self.config, track)
        now = self.clock()
        instruments.see(db, row['symbol'], now)
        if name == 'thesis_set':
            exact(args, {'thesis_id', 'assessment', 'summary', 'source_refs', 'expected_version'})
            thesis = theses.get(db, args['thesis_id'])
            if thesis['symbol'] != row['symbol']:
                raise Refused(FORBIDDEN, f"thesis {thesis['thesis_id']} is on {thesis['symbol']}; this Track "
                                         f"holds the research key of {row['symbol']}")
            theses.assess(self.ledger, db, thesis, args, now, caller=caller, track_id=track['id'])
        else:
            exact(args, set())
        targets, held = self.held(db)
        return research.view(db, row, self.ledger.get_meta(db, 'snapshot'), targets, held)

    def held(self, db):
        """`(targets, held)`: the latest decision's weights, and every symbol it weights above 0 or that
        still has a position."""
        latest = db.execute('SELECT body FROM decisions ORDER BY rowid DESC LIMIT 1').fetchone()
        targets = json.loads(latest[0])['weights'] if latest else {}
        return targets, {s for s, bps in targets.items() if bps} | set(self.positions(db))

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
        captured(body['source_refs'])
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
        instruments.verify(self.ledger, db, self.config, snapshot, self.clock())
        # One sample per trading session (the newest quote's New York date); the latest read wins.
        # Without any quote there is no session to value, so no sample.
        # A session date never moves backwards (a halted symbol's quote can be the newest one left).
        latest = db.execute('SELECT max(date) FROM valuations').fetchone()[0]
        if snapshot['date'] is not None and latest is not None:
            snapshot['date'] = max(snapshot['date'], latest)
            self.ledger.set_meta(db, 'snapshot', snapshot)
        if snapshot['date'] is not None:
            sample = {'date': snapshot['date'], 'at': snapshot['at'],
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
        filled = self.ledger.filled(db)
        for d in decisions:
            d['orders'] = [o | filled.get(o['broker_id'], {'filled_quantity': 0, 'filled_amount_usd': '0'})
                           for o in legs if o['decision_id'] == d['id']]
        targets, held = self.held(db)
        now, rows = self.clock(), []
        for i in instruments.listed(db):
            live = i['state'] == 'live'
            rows.append({'symbol': i['symbol'], 'state': i['state'], 'version': i['version'],
                         'held': i['state'] in instruments.COUNTED and i['symbol'] in held,
                         'key': i['body']['track_add']['idempotency_key'] if live else None,
                         'key_seq': i['key_seq'], 'issued_at': i['issued_at'], 'last_seen_at': i['last_seen_at'],
                         'stale': instruments.stale(i, now, self.config.lease_days),
                         'track_add': i['body']['track_add'] if live else None,
                         'added_at': i['body'].get('added_at'), 'reason': i['body'].get('reason')})
        counted = [i for i in rows if i['state'] in instruments.COUNTED]
        return {'snapshot': self.ledger.get_meta(db, 'snapshot'),
                'opening_positions': self.ledger.get_meta(db, 'opening_positions'),
                'instruments': rows, 'targets': targets,
                'theses': [{k: v for k, v in t.items() if k != 'body'} for t in theses.open_theses(db)],
                'limits': {'max_held': self.config.max_held, 'max_watched': self.config.max_watched,
                           'lease_days': self.config.lease_days,
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
