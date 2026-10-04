"""The invest ledger: one SQLite file, one cross-process lock and an append-only journal.

The session, lock and journal are the paper plugin's (`plugins/paper-trading/paper_trading/ledger.py`);
the tables are the multi-instrument shape of #2104 §3.7. Theses, issued keys and the research lease
extend `instruments` in a later slice.
"""
from contextlib import contextmanager
from datetime import datetime, timezone
from decimal import Decimal
import fcntl
import hashlib
import json
from pathlib import Path
import sqlite3
import threading

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS instruments (
    symbol TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('pending', 'live', 'dropped')),
    key_seq INTEGER NOT NULL, issued_at TEXT, last_seen_at TEXT,
    version INTEGER NOT NULL, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS decisions (
    id TEXT PRIMARY KEY, body TEXT NOT NULL, state TEXT NOT NULL, error TEXT, created_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS orders (
    id TEXT PRIMARY KEY, decision_id TEXT NOT NULL REFERENCES decisions(id), symbol TEXT NOT NULL,
    request TEXT NOT NULL, state TEXT NOT NULL, broker_id TEXT UNIQUE, broker_status TEXT, error TEXT,
    UNIQUE (decision_id, symbol));
CREATE TABLE IF NOT EXISTS fills (id TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS valuations (date TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS journal (
    seq INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL);
PRAGMA user_version=1;
"""


def encoded(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"), allow_nan=False)


def digest(value):
    return hashlib.sha256(encoded(value).encode()).hexdigest()


def bound(db, key, body, what):
    """Pin `body` under meta `key` on first open; refuse a different one later."""
    previous = Ledger.get_meta(db, key)
    if previous is not None and previous != body:
        raise ValueError(f'ledger {what} cannot be changed')
    Ledger.set_meta(db, key, body)


class Ledger:
    def __init__(self, root, config):
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.lock = threading.RLock()
        with self.session() as db:
            if db.execute("PRAGMA user_version").fetchone()[0] not in (0, 1):
                raise ValueError("unsupported invest ledger version")
            db.executescript(SCHEMA)
            bound(db, 'binding', {'account_no': config.account_no,
                                  'portfolio_track_id': config.portfolio_track_id}, 'account/Track binding')
            bound(db, 'execution_binding', {'oauth_client_id': config.oauth_client_id,
                                            'broker_home': config.broker_home}, 'execution binding')
            # The process might have died after broker acceptance but before the acknowledgement.
            for row in db.execute("SELECT id FROM orders WHERE state='submitting'").fetchall():
                db.execute("UPDATE orders SET state='unknown' WHERE id=?", (row[0],))
                self.event(db, "submission_unknown", {"order_id": row[0]})

    @contextmanager
    def session(self):
        with self.lock, (self.root / ".operation.lock").open("a") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            db = sqlite3.connect(self.root / "ledger.sqlite3", timeout=20)
            db.row_factory = sqlite3.Row
            db.execute("PRAGMA foreign_keys=ON")
            db.execute("PRAGMA synchronous=FULL")
            try:
                with db:
                    yield db
            finally:
                db.close()

    @staticmethod
    def get_meta(db, key):
        row = db.execute("SELECT body FROM meta WHERE key=?", (key,)).fetchone()
        return json.loads(row[0]) if row else None

    @staticmethod
    def set_meta(db, key, body):
        db.execute("INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET body=excluded.body",
                   (key, encoded(body)))

    @staticmethod
    def event(db, kind, body):
        db.execute("INSERT INTO journal(at,kind,body) VALUES (?,?,?)",
                   (datetime.now(timezone.utc).isoformat(), kind, encoded(body)))

    @staticmethod
    def decisions(db):
        return [dict(row) | {"body": json.loads(row["body"])}
                for row in db.execute("SELECT * FROM decisions ORDER BY rowid")]

    @staticmethod
    def decision(db, key):
        row = db.execute("SELECT * FROM decisions WHERE id=?", (key,)).fetchone()
        if row is None:
            raise ValueError(f"unknown decision {key!r}")
        return dict(row) | {"body": json.loads(row["body"])}

    @staticmethod
    def orders(db, decision_id=None):
        rows = (db.execute("SELECT * FROM orders WHERE decision_id=? ORDER BY rowid", (decision_id,))
                if decision_id is not None else db.execute("SELECT * FROM orders ORDER BY rowid"))
        return [dict(row) | {"request": json.loads(row["request"])} for row in rows]

    @staticmethod
    def fills(db):
        return [json.loads(row[0]) for row in db.execute("SELECT body FROM fills ORDER BY id")]

    @staticmethod
    def filled(db):
        """`{broker order id: {filled_quantity, filled_amount_usd}}` over every persisted fill, so
        totals never depend on how many fills a view lists. Both sum in Python: amounts as exact
        decimals, since an SQL REAL sum would round."""
        totals = {}
        for order_id, quantity, price in db.execute(
                "SELECT json_extract(body,'$.order_id'), json_extract(body,'$.quantity'), "
                "json_extract(body,'$.price') FROM fills"):
            total = totals.setdefault(order_id, [0, Decimal(0)])
            total[0] += quantity
            total[1] += quantity * Decimal(price)
        return {k: {'filled_quantity': q, 'filled_amount_usd': str(a)} for k, (q, a) in totals.items()}

    def decide(self, db, key, state, error=None):
        row = self.decision(db, key)
        if (state, error) == (row["state"], row["error"]):
            return
        db.execute("UPDATE decisions SET state=?,error=? WHERE id=?", (state, error, key))
        self.event(db, "decision_state", {"decision_id": key, "state": state, "error": error})

    def order(self, db, key, state, error=None, broker_id=None, broker_status=None):
        row = db.execute("SELECT * FROM orders WHERE id=?", (key,)).fetchone()
        identity = broker_id or row["broker_id"]
        status = broker_status or row["broker_status"]
        if (state, error, identity, status) == (row["state"], row["error"], row["broker_id"], row["broker_status"]):
            return
        db.execute("UPDATE orders SET state=?,error=?,broker_id=?,broker_status=? WHERE id=?",
                   (state, error, identity, status, key))
        self.event(db, "order_state", {"order_id": key, "state": state, "error": error,
                                       "broker_id": identity, "broker_status": status})
