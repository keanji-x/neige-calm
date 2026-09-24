"""Human-approved strategy snapshots, separate from an account's connection.

All production entry points acquire the strategy lock before any ledger lock.
The operator and the polling process therefore see one approved configuration
for an entire operation, including a broker submission.
"""
from contextlib import contextmanager
from dataclasses import asdict
import fcntl
import json
from pathlib import Path
import sqlite3
import tempfile
import threading

from .config import Config, StrategyConfig, exact
from .engine import Engine, utc_now
from .ledger import digest, encoded
from .portfolio import TERMINAL


TOOLS = frozenset(("paper.strategy", "paper.ingest", "paper.decide", "paper.review",
                   "paper.pause", "paper.refresh", "paper.status", "paper.journal"))


def track_id(value):
    if not isinstance(value, str) or not value.strip() or len(value) > 256:
        raise ValueError("host-provided Track context required")
    return value


class Portfolio:
    def __init__(self, root, account, broker, clock=None):
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.account = account
        self.broker = broker
        self.clock = clock or utc_now
        self.lock = threading.RLock()
        with self.session() as db:
            version = db.execute("PRAGMA user_version").fetchone()[0]
            if version not in (0, 1):
                raise ValueError("unsupported strategy store version")
            db.executescript("""
                CREATE TABLE IF NOT EXISTS account (id INTEGER PRIMARY KEY CHECK(id=1), number TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS proposals (
                    revision TEXT PRIMARY KEY, track_id TEXT NOT NULL, settings TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS heads (
                    track_id TEXT PRIMARY KEY, revision TEXT NOT NULL REFERENCES proposals(revision));
                CREATE TABLE IF NOT EXISTS active (
                    id INTEGER PRIMARY KEY CHECK(id=1), revision TEXT NOT NULL REFERENCES proposals(revision));
                CREATE TABLE IF NOT EXISTS audit (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL);
                PRAGMA user_version=1;
            """)
            row = db.execute("SELECT number FROM account WHERE id=1").fetchone()
            if row is not None and row[0] != account.account_no:
                raise ValueError("strategy account binding cannot be changed")
            db.execute("INSERT OR IGNORE INTO account VALUES (1,?)", (account.account_no,))
            self.legacy_binding(db)

    @contextmanager
    def session(self):
        with self.lock, (self.root / '.strategy.lock').open('a') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            db = sqlite3.connect(self.root / 'strategy.sqlite3', timeout=20)
            db.row_factory = sqlite3.Row
            db.execute('PRAGMA foreign_keys=ON')
            db.execute('PRAGMA synchronous=FULL')
            try:
                with db:
                    yield db
            finally:
                db.close()

    def proposal(self, db, revision):
        row = db.execute('SELECT * FROM proposals WHERE revision=?', (revision,)).fetchone()
        if row is None:
            raise ValueError('unknown strategy proposal')
        settings = StrategyConfig.parse(json.loads(row['settings'])).json()
        expected = digest({'version': 1, 'account_no': self.account.account_no,
                           'track_id': row['track_id'], 'settings': settings})
        if expected != row['revision']:
            raise ValueError('strategy snapshot integrity mismatch')
        return {'revision': row['revision'], 'track_id': row['track_id'], 'settings': settings}

    def active(self, db):
        row = db.execute('SELECT revision FROM active WHERE id=1').fetchone()
        return self.proposal(db, row[0]) if row else None

    def legacy_binding(self, db):
        if self.active(db) is not None or not (self.root / 'ledger.sqlite3').exists():
            return None
        uri = (self.root / 'ledger.sqlite3').resolve().as_uri() + '?mode=ro'
        with sqlite3.connect(uri, uri=True) as legacy:
            if legacy.execute('PRAGMA user_version').fetchone()[0] != 1:
                raise ValueError('unsupported legacy paper ledger version')
            row = legacy.execute("SELECT body FROM meta WHERE key='binding'").fetchone()
            if row is None:
                raise ValueError('legacy ledger binding missing; inspect before migration')
            binding = json.loads(row[0])
            exact(binding, {'account_no', 'owner_track_id'})
            track_id(binding['owner_track_id'])
            if binding['account_no'] != self.account.account_no:
                raise ValueError('legacy account binding does not match connection')
            return binding

    def authorize(self, db, track):
        track_id(track)
        active = self.active(db)
        legacy = self.legacy_binding(db)
        owner = active['track_id'] if active else legacy['owner_track_id'] if legacy else None
        if owner is not None and track != owner:
            raise ValueError('this paper account is bound to another Track')
        return active

    def settings(self, db, track):
        active = self.authorize(db, track)
        row = db.execute('SELECT revision FROM heads WHERE track_id=?', (track,)).fetchone()
        proposal = self.proposal(db, row[0]) if row else None
        legacy = self.legacy_binding(db)
        phase = ('migration_required' if legacy else 'awaiting_approval' if proposal and
                 (not active or proposal['revision'] != active['revision']) else
                 'approved' if active else 'unconfigured')
        return {'phase': phase, 'account_no': self.account.account_no,
                'active': active, 'proposal': proposal}

    def engine(self, active):
        config = StrategyConfig.parse(active['settings']).bind(self.account, active['track_id'])
        return Engine(self.root, config, self.broker, clock=self.clock, strategy_revision=active['revision'])

    def status(self, db, track):
        settings = self.settings(db, track)
        if settings['active']:
            engine = self.engine(settings['active'])
            with engine.ledger.session() as ledger:
                state = engine.status(ledger)
        else:
            state = {'mode': 'supervised_paper', 'paused': True, 'snapshot': None, 'error': None,
                     'alerts': [], 'decisions': [], 'trades': [], 'fills': [], 'reviews': [], 'journal': []}
        return state | {'strategy': settings}

    def save_proposal(self, db, track, strategy):
        settings = strategy.json()
        revision = digest({'version': 1, 'account_no': self.account.account_no,
                           'track_id': track, 'settings': settings})
        db.execute('INSERT OR IGNORE INTO proposals VALUES (?,?,?)', (revision, track, encoded(settings)))
        db.execute('INSERT INTO heads VALUES (?,?) ON CONFLICT(track_id) DO UPDATE SET revision=excluded.revision',
                   (track, revision))
        return revision

    def call(self, track, name, args):
        if name not in TOOLS:
            raise ValueError('unknown tool')
        with self.session() as db:
            active = self.authorize(db, track)
            if name == 'paper.strategy':
                if self.legacy_binding(db):
                    raise ValueError('legacy strategy import required before proposing changes')
                self.save_proposal(db, track, StrategyConfig.parse(args))
                return self.status(db, track)
            if name in ('paper.status', 'paper.journal'):
                exact(args, set())
                return self.status(db, track)
            if active is None:
                raise ValueError('human strategy approval required; propose with paper.strategy first')
            return self.engine(active).call(track, name, args)

    def process_once(self):
        with self.session() as db:
            active = self.active(db)
            if active:
                state = self.engine(active).process_once()
                return [(active['track_id'], state | {'strategy': self.settings(db, active['track_id'])})]
            legacy = self.legacy_binding(db)
            tracks = ([legacy['owner_track_id']] if legacy else
                      [row[0] for row in db.execute('SELECT track_id FROM heads ORDER BY track_id')])
            return [(track, self.status(db, track)) for track in tracks]

    def approval_preview(self, revision):
        with self.session() as db:
            proposal = self.proposal(db, revision)
            self.authorize(db, proposal['track_id'])
            if self.legacy_binding(db):
                raise ValueError('explicit legacy import required')
            self.check_head(db, proposal)
            return {'account_no': self.account.account_no, **proposal}

    @staticmethod
    def check_head(db, proposal):
        row = db.execute('SELECT revision FROM heads WHERE track_id=?', (proposal['track_id'],)).fetchone()
        if row is None or row[0] != proposal['revision']:
            raise ValueError('strategy proposal was superseded; review the current proposal')

    def approve(self, revision):
        with self.session() as db:
            proposal = self.proposal(db, revision)
            active = self.authorize(db, proposal['track_id'])
            if self.legacy_binding(db):
                raise ValueError('explicit legacy import required')
            self.check_head(db, proposal)
            self.broker.identity(self.account.account_no)
            if active and active['revision'] == revision:
                return proposal
            if active:
                state = self.engine(active).process_once()
            else:
                # Check a new account with the actual reconciliation engine, but
                # create no live ledger before the approval transaction commits.
                with tempfile.TemporaryDirectory(prefix='approval-', dir=self.root) as temporary:
                    config = StrategyConfig.parse(proposal['settings']).bind(self.account, proposal['track_id'])
                    state = Engine(temporary, config, self.broker, clock=self.clock).process_once()
            if state['error']:
                raise ValueError('resolve account reconciliation before strategy approval: ' + state['error'])
            if any(d['state'] not in TERMINAL for d in state['decisions']) or any(t['quantity'] for t in state['trades']):
                raise ValueError('resolve unresolved decisions or open trades before changing strategy')
            self.activate(db, proposal, 'strategy_approved')
            return proposal

    def activate(self, db, proposal, kind):
        previous = self.active(db)
        db.execute('INSERT INTO active VALUES (1,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision',
                   (proposal['revision'],))
        db.execute('INSERT INTO audit(at,kind,body) VALUES (?,?,?)',
                   (self.clock().isoformat(), kind, encoded({'previous': previous, 'approved': proposal})))

    def legacy_preview(self, values):
        config = Config.parse(values)
        with self.session() as db:
            binding = self.legacy_binding(db)
            if binding is None:
                raise ValueError('no legacy ledger requires import')
            if (binding != {'account_no': config.account_no, 'owner_track_id': config.owner_track_id}
                    or any(getattr(config, key) != getattr(self.account, key)
                           for key in ('account_no', 'broker_home', 'cli_path'))):
                raise ValueError('legacy configuration binding does not match account/Track/connection')
            strategy = StrategyConfig.from_legacy(config)
            return {'account_no': config.account_no, 'track_id': config.owner_track_id,
                    'settings': strategy.json(), 'legacy_config_digest': digest(asdict(config))}

    def import_legacy(self, values):
        preview = self.legacy_preview(values)
        with self.session() as db:
            binding = self.legacy_binding(db)
            if binding is None or binding['owner_track_id'] != preview['track_id']:
                raise ValueError('legacy binding changed since preview')
            self.broker.identity(self.account.account_no)
            revision = self.save_proposal(db, preview['track_id'], StrategyConfig.parse(preview['settings']))
            self.activate(db, self.proposal(db, revision), 'legacy_strategy_imported')
            return self.proposal(db, revision)

    def operator_preview(self, key, cancel=False):
        with self.session() as db:
            active = self.active(db)
            if active is None:
                raise ValueError('human strategy approval required')
            args, preview = self.engine(active).operator_preview(key, cancel)
            return {'arguments': args, 'strategy_revision': active['revision']}, preview

    def operator_confirm(self, key, request, code, cancel=False):
        with self.session() as db:
            active = self.active(db)
            if active is None or request.get('strategy_revision') != active['revision']:
                raise ValueError('approved strategy changed since preview; review again')
            exact(request, {'arguments', 'strategy_revision'})
            return self.engine(active).operator_confirm(key, request['arguments'], code, cancel)
