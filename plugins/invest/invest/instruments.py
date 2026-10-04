"""Covered instruments: `pending` until the loop verifies a US quote, then `live`; `dropped` otherwise.

Only `pending` and `live` count toward the limits. This slice owns the ledger shape and verification;
the agent-facing add/set/rm, issued research keys and the lease build on it (#2104 §3.3).
"""
from .ledger import encoded

COUNTED = ('pending', 'live')


def admit(ledger, db, symbol, now):
    """Add `symbol` as `pending`; a dropped symbol comes back pending, a counted one is unchanged."""
    row = db.execute('SELECT state, version FROM instruments WHERE symbol=?', (symbol,)).fetchone()
    if row is not None and row['state'] in COUNTED:
        return
    body = encoded({'added_at': now.isoformat()})
    if row is None:
        db.execute('INSERT INTO instruments(symbol,state,key_seq,version,body) VALUES (?,?,?,?,?)',
                   (symbol, 'pending', 0, 1, body))
    else:
        db.execute("UPDATE instruments SET state='pending', version=version+1, body=? WHERE symbol=?",
                   (body, symbol))
    ledger.event(db, 'instrument_added', {'symbol': symbol})


def counted(db):
    """`{symbol: state}` of every pending or live instrument."""
    return {row['symbol']: row['state']
            for row in db.execute("SELECT symbol, state FROM instruments WHERE state IN ('pending','live')")}


def listed(db):
    return [dict(row) for row in db.execute(
        'SELECT symbol, state, version, body FROM instruments ORDER BY symbol')]


def verify(ledger, db, snapshot):
    """A pending symbol with a broker quote goes live. Without one it is dropped, unless it is held,
    in which case reconciliation has already refused the observation."""
    for symbol, state in counted(db).items():
        if state != 'pending':
            continue
        if symbol in snapshot['quotes']:
            db.execute("UPDATE instruments SET state='live', version=version+1 WHERE symbol=?", (symbol,))
            ledger.event(db, 'instrument_verified', {'symbol': symbol})
        elif symbol not in snapshot['positions']:
            reason = 'the broker returned no US quote for this symbol'
            db.execute("UPDATE instruments SET state='dropped', version=version+1, body=? WHERE symbol=?",
                       (encoded({'reason': reason}), symbol))
            ledger.event(db, 'instrument_dropped', {'symbol': symbol, 'reason': reason})
