"""The invest App's tools and its one reconcile/submit loop, the only broker writer.

The portfolio Track decides, executes, covers instruments and raises theses; a research Track that the
portfolio Track added assesses its own symbol's theses and never trades (#2104 §3.1, §3.6)."""
from datetime import datetime, timedelta, timezone
import json

from . import arguments, instruments, research, theses
from .config import timestamp
from .errors import CONFLICT, FORBIDDEN, INVALID, NOT_FOUND, UNKNOWN_TOOL, Refused, served
from .execution import FINAL, IN_FLIGHT, advance, expire
from .ledger import Ledger, encoded
from .reconcile import reconcile, validate_snapshot
from .symbols import to_sdk

ROLES = {'portfolio_status': ('planner', 'worker'), 'decision_add': ('planner',), 'execution_add': ('worker',),
         'instrument_add': ('planner',), 'instrument_set': ('planner',), 'instrument_rm': ('planner',),
         'thesis_add': ('planner',), 'thesis_rm': ('planner',),
         'instrument_status': ('planner',), 'thesis_set': ('planner',)}
TOOLS = frozenset(ROLES)
RESEARCH_TOOLS = frozenset(('instrument_status', 'thesis_set'))  # attested by provenance (§3.3)
PORTFOLIO_TOOLS = TOOLS - RESEARCH_TOOLS  # only the portfolio Track
VIEWS = frozenset(('portfolio_status', 'instrument_status'))
HISTORY = 260  # about one year of trading sessions: one valuation sample per session
FILL_LOG = 500  # the fill table's rows; agent responses carry the latest 200


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
                raise Refused(UNKNOWN_TOOL, f'unknown tool; the invest tools are {sorted(map(served, TOOLS))}',
                              'unknown_tool')
            track = research.context(track)
            if name in PORTFOLIO_TOOLS and track['id'] != self.config.portfolio_track_id:
                raise Refused(FORBIDDEN, 'only the portfolio Track may call this tool; research Tracks never trade',
                              'portfolio_track_only')
            role = caller.get('role') if isinstance(caller, dict) else None
            if role not in ('planner', 'worker') or not all(isinstance(caller.get(k), str) and caller[k]
                                                            for k in ('card_id', 'session_id')):
                raise Refused(FORBIDDEN, 'host-provided agent identity required', 'agent_identity')
            if role not in ROLES[name]:
                raise Refused(FORBIDDEN, f"requires {' or '.join(r.capitalize() for r in ROLES[name])} identity",
                              'role', roles=list(ROLES[name]))
            # The one argument boundary: nothing unchecked reaches a lookup or the ledger.
            args = arguments.parse(name, args)
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
                return self.status(db) | result
        except Refused as error:
            raise error.named(name) from None
        except ValueError as error:
            raise Refused(INVALID, f'{served(name)}: {error}', 'invalid_argument') from None

    def instrument_write(self, db, name, args, caller):
        """`instrument_add`, `instrument_set` (renew the issued key) and `instrument_rm`."""
        symbol, message, now = args['symbol'], args['message'], self.clock()
        if name == 'instrument_set':
            return {'track_add': instruments.renew(self.ledger, db, self.config, symbol, args['expected_version'],
                                                   now, message=message, caller=caller)}
        _targets, held = self.held(db)
        if name == 'instrument_rm':
            instruments.current(db, symbol, args['expected_version'])
            if symbol in held:
                raise Refused(CONFLICT, f'{symbol} is held (weighted above 0 by the latest decision, or in a '
                                        'position); sell it to 0 and let the sale settle first',
                              'instrument_held', symbol=symbol)
            instruments.drop(self.ledger, db, symbol, f'removed: {message}', caller=caller)
            theses.retire_open(self.ledger, db, symbol, now, message, caller=caller)
            return {}
        row = instruments.get(db, symbol)
        if row is not None and row['state'] in instruments.COUNTED:
            return {}  # already covered
        watched = sum(s not in held for s in instruments.counted(db))
        if watched >= self.config.max_watched:
            raise Refused(CONFLICT, f'{watched} symbols are watched (covered, not held), at max_watched '
                                    f'{self.config.max_watched}; remove one with plugin_invest_instrument_rm first',
                          'max_watched', watched=watched, max_watched=self.config.max_watched)
        instruments.admit(self.ledger, db, symbol, now, message=message, caller=caller)
        return {}

    def research_call(self, db, track, name, args, caller):
        """An attested research call: stamp the lease, act on the caller's own symbol, return its view."""
        row = research.attest(db, self.config, track)
        now = self.clock()
        instruments.see(db, row['symbol'], now)
        if name == 'thesis_set':
            thesis = theses.get(db, args['thesis_id'])
            if thesis['symbol'] != row['symbol']:
                raise Refused(FORBIDDEN, f"thesis {thesis['thesis_id']} is on {thesis['symbol']}; this Track "
                                         f"holds the research key of {row['symbol']}", 'not_attested')
            theses.assess(self.ledger, db, thesis, args, now, caller=caller, track_id=track['id'])
        targets, held = self.held(db)
        return research.view(db, row, self.ledger.get_meta(db, 'snapshot'), targets, held,
                             self.config.portfolio_track_id)

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
        key = args['decision_id']
        body = {k: args[k] for k in ('weights', 'message', 'source_refs', 'valid_until')}
        old = db.execute('SELECT body FROM decisions WHERE id=?', (key,)).fetchone()
        if old is not None:
            if old[0] != encoded(body):
                raise Refused(CONFLICT, 'decision_id already names a different decision', 'decision_id_taken')
            return
        now = self.clock()
        if not now < timestamp(body['valid_until']) <= now + timedelta(hours=24):
            raise ValueError('valid_until must fall within the next 24 hours')
        if any(d['state'] not in FINAL for d in self.ledger.decisions(db)):
            raise Refused(CONFLICT, 'resolve the current decision before adding another', 'decision_unresolved')
        self.check_bounds(db, body['weights'])
        db.execute('INSERT INTO decisions(id,body,state,created_at) VALUES (?,?,?,?)',
                   (key, encoded(body), 'queued', now.isoformat()))
        self.ledger.event(db, 'decision_added', {'decision_id': key, 'caller': caller, **body})

    def check_bounds(self, db, weights):
        config, covered = self.config, instruments.counted(db)
        for symbol, bps in weights.items():
            if symbol not in covered:
                raise Refused(NOT_FOUND, f'{symbol} is not a covered instrument', 'unknown_instrument', symbol=symbol)
            if bps and covered[symbol] != 'live':
                raise Refused(CONFLICT, f'{symbol} is {covered[symbol]}; a weight above 0 requires a live instrument',
                              'instrument_not_live', symbol=symbol, state=covered[symbol])
            if bps > config.max_weight_bps:
                raise ValueError(f'{symbol} weight {bps} exceeds max_weight_bps {config.max_weight_bps}')
        if sum(weights.values()) > 10000 - config.cash_buffer_bps:
            raise ValueError(f'weights sum to {sum(weights.values())}, above 10000 - cash_buffer_bps '
                             f'({10000 - config.cash_buffer_bps})')
        held_after = {s for s, bps in weights.items() if bps} | set(self.positions(db))
        if len(held_after) > config.max_held:
            raise Refused(CONFLICT, f'{len(held_after)} symbols would be held (weighted or still in a position), '
                                    f'above max_held {config.max_held}; sell first, then buy once the sells settle',
                          'max_held', held=len(held_after), max_held=config.max_held)

    def execution_add(self, db, args, caller):
        """Record the Worker's execution request; the background loop performs it."""
        key = args['decision_id']
        if db.execute('SELECT 1 FROM decisions WHERE id=?', (key,)).fetchone() is None:
            raise Refused(NOT_FOUND, f'unknown decision {key!r}; read plugin_invest_portfolio_status',
                          'unknown_decision', decision_id=key)
        decision = self.ledger.decision(db, key)
        if decision['state'] == 'requested':
            return
        if decision['state'] != 'queued':
            raise Refused(CONFLICT, f"decision {key} is {decision['state']}, not awaiting execution; "
                                    'read plugin_invest_portfolio_status', 'decision_state', state=decision['state'])
        if timestamp(decision['body']['valid_until']) <= self.clock():
            raise Refused(CONFLICT, f'decision {key} expired; the Planner must add a new decision', 'decision_state',
                          state='expired')
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
