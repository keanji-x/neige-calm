"""Covered instruments and their research keys (#2104 §3.3).

A symbol is `pending` until the loop verifies a US quote, then `live`; it is `dropped` when removed or
refused by verification. Only `pending` and `live` count toward the limits.

Going live issues the symbol's next research key `invest-<VENUE>-<CODE>-<n>`, and renewal issues the
one after: `n` never decreases, so a key is never reused. The ledger keeps the exact
`neige_track_add` arguments issued under the current key, so every retry passes the same bytes.
`last_seen_at` is access metadata of the current key: it never bumps `version` and never changes
authority.
"""
from datetime import timedelta
import json
import re

from .config import timestamp, version
from .ledger import encoded

COUNTED = ('pending', 'live')
BIND_WINDOW = timedelta(minutes=120)  # a new key's Track must make its first call this soon
KEY = re.compile(r'invest-([A-Z]{2,8})-([A-Z0-9._-]{1,32})-([1-9][0-9]{0,8})')


def research_key(symbol, n):
    venue, code = symbol.split(':', 1)
    return f'invest-{venue}-{code}-{n}'


def parse_key(key):
    """`(symbol, n)` of a research key, else None: the venue has no `-`, and `n` follows the last `-`."""
    match = KEY.fullmatch(key) if isinstance(key, str) else None
    return (f'{match.group(1)}:{match.group(2)}', int(match.group(3))) if match else None


def track_add(config, symbol, n):
    """The `neige_track_add` arguments of key `n`, which the portfolio Planner passes verbatim."""
    key = research_key(symbol, n)
    return {'recipe_id': config.instrument_recipe_id, 'title': f'{symbol} 研究', 'idempotency_key': key,
            'text': (f'Cover {symbol} as its invest research Track (key {key}). Set up the weekly calendar '
                     'entry your recipe defines, then run its research step now. Start every step with '
                     'plugin_invest_instrument_status; if it answers superseded, close this Track.'),
            'message': f'invest: open the research Track for {symbol} under {key}'}


def get(db, symbol):
    row = db.execute('SELECT * FROM instruments WHERE symbol=?', (symbol,)).fetchone()
    return dict(row) | {'body': json.loads(row['body'])} if row else None


def listed(db):
    return [dict(row) | {'body': json.loads(row['body'])}
            for row in db.execute('SELECT * FROM instruments ORDER BY symbol')]


def counted(db):
    """`{symbol: state}` of every pending or live instrument."""
    return {row['symbol']: row['state']
            for row in db.execute("SELECT symbol, state FROM instruments WHERE state IN ('pending','live')")}


def admit(ledger, db, symbol, now, **audit):
    """Add `symbol` as `pending`; a dropped symbol comes back pending, a counted one is unchanged."""
    row = get(db, symbol)
    if row is not None and row['state'] in COUNTED:
        return
    body = encoded({'added_at': now.isoformat()})
    if row is None:
        db.execute('INSERT INTO instruments(symbol,state,key_seq,version,body) VALUES (?,?,?,?,?)',
                   (symbol, 'pending', 0, 1, body))
    else:
        db.execute("UPDATE instruments SET state='pending', version=version+1, body=? WHERE symbol=?",
                   (body, symbol))
    ledger.event(db, 'instrument_added', {'symbol': symbol, **audit})


def issue(ledger, db, config, symbol, now):
    """Make `symbol` live under its next key; the previous key, if any, is superseded."""
    row = get(db, symbol)
    n = row['key_seq'] + 1
    issued = track_add(config, symbol, n)
    body = {k: v for k, v in row['body'].items() if k != 'reason'} | {'track_add': issued}
    db.execute("UPDATE instruments SET state='live', key_seq=?, issued_at=?, last_seen_at=NULL, "
               "version=version+1, body=? WHERE symbol=?", (n, now.isoformat(), encoded(body), symbol))
    ledger.event(db, 'key_issued', {'symbol': symbol, 'key': issued['idempotency_key']})
    return issued


def verify(ledger, db, config, snapshot, now):
    """A pending symbol with a broker quote goes live under its next key. Without one it is dropped,
    unless it is held, in which case reconciliation has already refused the observation."""
    for symbol, state in counted(db).items():
        if state != 'pending':
            continue
        if symbol in snapshot['quotes']:
            issue(ledger, db, config, symbol, now)
        elif symbol not in snapshot['positions']:
            drop(ledger, db, symbol, 'the broker returned no US quote for this symbol')


def drop(ledger, db, symbol, reason, **audit):
    db.execute("UPDATE instruments SET state='dropped', version=version+1, body=? WHERE symbol=?",
               (encoded({'reason': reason}), symbol))
    ledger.event(db, 'instrument_dropped', {'symbol': symbol, 'reason': reason, **audit})


def current(db, symbol, expected_version):
    """The counted instrument a `set` or `rm` names, at the caller's `expected_version`."""
    row = get(db, symbol)
    if row is None or row['state'] not in COUNTED:
        raise ValueError(f'{symbol} is not a covered instrument; read portfolio_status')
    version(expected_version, row['version'])
    return row


def renew(ledger, db, config, symbol, expected_version, now, **audit):
    """Replace a live symbol's issued key with the next one, returning the new `track_add`."""
    row = current(db, symbol, expected_version)
    if row['state'] != 'live':
        raise ValueError(f'{symbol} is {row["state"]}; only a live instrument has a research key to renew')
    issued = issue(ledger, db, config, symbol, now)
    ledger.event(db, 'instrument_renewed', {'symbol': symbol, **audit})
    return issued


def see(db, symbol, now):
    """Stamp an attested call: access metadata, so neither `version` nor the journal changes."""
    db.execute('UPDATE instruments SET last_seen_at=? WHERE symbol=?', (now.isoformat(), symbol))


def stale(row, now, lease_days):
    """A live symbol without a working research Track: its current key was not seen within the bind
    window of issue, or has not been seen for `lease_days`."""
    if row['state'] != 'live':
        return False
    if row['last_seen_at'] is None:
        return now - timestamp(row['issued_at']) > BIND_WINDOW
    return now - timestamp(row['last_seen_at']) > timedelta(days=lease_days)
