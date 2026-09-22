"""The broker ledger is authoritative; report tables are disposable projections."""
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import json
from pathlib import Path
import sqlite3
import threading


def encoded(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"), allow_nan=False)


def digest(value):
    return hashlib.sha256(encoded(value).encode()).hexdigest()


class Ledger:
    def __init__(self, root, config):
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.lock = threading.RLock()
        with self.session() as db:
            version = db.execute("PRAGMA user_version").fetchone()[0]
            if version not in (0, 1):
                raise ValueError("unsupported paper ledger version")
            db.executescript("""
                CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, body TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS sources (id TEXT PRIMARY KEY, body TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS decisions (
                    id TEXT PRIMARY KEY, body TEXT NOT NULL, state TEXT NOT NULL,
                    broker_id TEXT UNIQUE, broker_status TEXT, error TEXT,
                    created_at TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS fills (id TEXT PRIMARY KEY, body TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS journal (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT NOT NULL,
                    kind TEXT NOT NULL, body TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS reviews (id TEXT PRIMARY KEY, body TEXT NOT NULL);
                PRAGMA user_version=1;
            """)
            binding = {"account_no": config.account_no, "owner_track_id": config.owner_track_id}
            previous = self.get_meta(db, "binding")
            if previous is not None and previous != binding:
                raise ValueError("ledger account/Track binding cannot be changed")
            self.set_meta(db, "binding", binding)
            if self.get_meta(db, "paused") is None:
                self.set_meta(db, "paused", False)
            # The process might have died after broker acceptance but before ack.
            for row in db.execute("SELECT id FROM decisions WHERE state='submitting'").fetchall():
                db.execute("UPDATE decisions SET state='unknown' WHERE id=?", (row[0],))
                self.event(db, "submission_unknown", {"decision_id": row[0]})

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
                for row in db.execute("SELECT * FROM decisions ORDER BY created_at,id")]

    @staticmethod
    def decision(db, key):
        row = db.execute("SELECT * FROM decisions WHERE id=?", (key,)).fetchone()
        if row is None:
            raise ValueError("unknown decision")
        return dict(row) | {"body": json.loads(row["body"])}

    @staticmethod
    def fills(db):
        return [json.loads(row[0]) for row in db.execute("SELECT body FROM fills ORDER BY id")]

    def change(self, db, key, state, error=None, broker_id=None, broker_status=None):
        row = self.decision(db, key)
        identity = broker_id or row["broker_id"]
        status = broker_status or row["broker_status"]
        if (state, error, identity, status) == (row["state"], row["error"], row["broker_id"], row["broker_status"]):
            return
        db.execute("UPDATE decisions SET state=?,error=?,broker_id=?,broker_status=? WHERE id=?",
                   (state, error, identity, status, key))
        self.event(db, "decision_state", {"decision_id": key, "state": state, "error": error,
                                          "broker_id": identity, "broker_status": status})
