"""Theses in the plugin ledger (#2104 §3.2): the portfolio raises one on a live symbol, the symbol's
research Track assesses it, and the portfolio retires it. Retiring is a verb, never an assessment."""
import json
import re

from . import instruments
from .config import captured, exact, text, version
from .ledger import encoded
from .symbols import canonical

OPEN_CAP = 3  # open theses per symbol
STANCES = ('bullish', 'bearish', 'neutral')
ASSESSMENTS = ('open', 'holding', 'at_risk', 'broken')
THESIS_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,63}')
RETIRED_SHOWN = 20  # the research Track's records keep the latest retired theses
CONTENT = ('stance', 'title', 'summary', 'body', 'source_refs')


def row_of(row):
    return {'thesis_id': row['id'], 'symbol': row['symbol'], 'assessment': row['assessment'],
            'version': row['version'], 'retired_at': row['retired_at'], **json.loads(row['body'])}


def get(db, thesis_id):
    row = db.execute('SELECT * FROM theses WHERE id=?', (thesis_id,)).fetchone()
    if row is None:
        raise ValueError(f'unknown thesis {thesis_id!r}; read the theses of portfolio_status or instrument_status')
    return row_of(row)


def open_theses(db, symbol=None):
    """Open theses, oldest first: of one symbol, or of every symbol."""
    query, params = 'SELECT * FROM theses WHERE retired_at IS NULL', ()
    if symbol is not None:
        query, params = query + ' AND symbol=?', (symbol,)
    return [row_of(row) for row in db.execute(query + ' ORDER BY rowid', params)]


def retired_theses(db, symbol, limit=RETIRED_SHOWN):
    """The latest retired theses of `symbol`, newest first."""
    return [row_of(row) for row in db.execute(
        'SELECT * FROM theses WHERE symbol=? AND retired_at IS NOT NULL ORDER BY retired_at DESC, rowid DESC '
        'LIMIT ?', (symbol, limit))]


def add(ledger, db, args, now, **audit):
    """Raise a thesis on a live symbol; repeating the same thesis returns it."""
    exact(args, {'thesis_id', 'symbol', 'stance', 'title', 'summary', 'body', 'source_refs'})
    thesis_id = args['thesis_id']
    if not isinstance(thesis_id, str) or not THESIS_ID.fullmatch(thesis_id):
        raise ValueError('thesis_id must be 1-64 lowercase letters, digits or hyphens, not starting with a hyphen')
    symbol = canonical(args['symbol'])
    if args['stance'] not in STANCES:
        raise ValueError(f'stance must be one of {", ".join(STANCES)}')
    content = {'stance': args['stance'], 'title': text(args['title'], 'title', 110),
               'summary': text(args['summary'], 'summary', 500), 'body': text(args['body'], 'body', 6000),
               'source_refs': captured(args['source_refs'])}
    old = db.execute('SELECT * FROM theses WHERE id=?', (thesis_id,)).fetchone()
    if old is not None:
        old = row_of(old)
        if old['symbol'] == symbol and old['retired_at'] is None and {k: old[k] for k in CONTENT} == content:
            return
        raise ValueError(f'thesis_id {thesis_id} already names another thesis; choose a new thesis_id')
    instrument = instruments.get(db, symbol)
    if instrument is None or instrument['state'] != 'live':
        state = instrument['state'] if instrument else 'not covered'
        raise ValueError(f'{symbol} is {state}; a thesis needs a live instrument')
    if len(open_theses(db, symbol)) >= OPEN_CAP:
        raise ValueError(f'{symbol} has at most {OPEN_CAP} open theses; retire one with thesis_rm first')
    db.execute('INSERT INTO theses(id,symbol,assessment,version,retired_at,body) VALUES (?,?,?,?,?,?)',
               (thesis_id, symbol, 'open', 1, None, encoded(content | {'added_at': now.isoformat()})))
    ledger.event(db, 'thesis_added', {'thesis_id': thesis_id, 'symbol': symbol, **audit})


def assess(ledger, db, thesis, args, now, **audit):
    """The research assessment of one open thesis, under `expected_version`."""
    if thesis['retired_at'] is not None:
        raise ValueError(f"thesis {thesis['thesis_id']} is retired; assess an open thesis")
    if args['assessment'] not in ASSESSMENTS:
        raise ValueError(f'assessment must be one of {", ".join(ASSESSMENTS)}')
    version(args['expected_version'], thesis['version'])
    body = {k: thesis[k] for k in CONTENT + ('added_at',)} | {
        'summary': text(args['summary'], 'summary', 500), 'source_refs': captured(args['source_refs']),
        'assessed_at': now.isoformat()}
    db.execute('UPDATE theses SET assessment=?, version=version+1, body=? WHERE id=?',
               (args['assessment'], encoded(body), thesis['thesis_id']))
    ledger.event(db, 'thesis_assessed', {'thesis_id': thesis['thesis_id'], 'assessment': args['assessment'], **audit})


def retire(ledger, db, thesis_id, now, message, **audit):
    db.execute('UPDATE theses SET retired_at=?, version=version+1 WHERE id=? AND retired_at IS NULL',
               (now.isoformat(), thesis_id))
    ledger.event(db, 'thesis_retired', {'thesis_id': thesis_id, 'message': message, **audit})


def remove(ledger, db, args, now, **audit):
    """`thesis_rm`: retire one open thesis under `expected_version`."""
    exact(args, {'thesis_id', 'expected_version', 'message'})
    thesis = get(db, args['thesis_id'])
    if thesis['retired_at'] is not None:
        raise ValueError(f"thesis {thesis['thesis_id']} is already retired")
    version(args['expected_version'], thesis['version'])
    retire(ledger, db, thesis['thesis_id'], now, text(args['message'], 'message', 2000), **audit)


def retire_open(ledger, db, symbol, now, message, **audit):
    """Retire every open thesis of `symbol`: its instrument is being removed."""
    for thesis in open_theses(db, symbol):
        retire(ledger, db, thesis['thesis_id'], now, message, **audit)
