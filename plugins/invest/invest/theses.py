"""Theses in the plugin ledger (#2104 §3.2): the portfolio raises one on a live symbol, the symbol's
research Track assesses it, and the portfolio retires it. Retiring is a verb, never an assessment.

The portfolio owns a thesis's stance, title, summary, body and sources; an assessment never changes
them. The research Track owns the assessment and its own `research` summary and sources.
Every function takes arguments already checked by `arguments.parse`."""
import json

from . import instruments
from .config import version
from .errors import CONFLICT, NOT_FOUND, Refused
from .ledger import encoded

OPEN_CAP = 3  # open theses per symbol
RETIRED_SHOWN = 20  # the research Track's records keep the latest retired theses
CONTENT = ('stance', 'title', 'summary', 'body', 'source_refs')  # portfolio-owned


def row_of(row):
    return {'thesis_id': row['id'], 'symbol': row['symbol'], 'assessment': row['assessment'],
            'version': row['version'], 'retired_at': row['retired_at'], **json.loads(row['body'])}


def get(db, thesis_id):
    row = db.execute('SELECT * FROM theses WHERE id=?', (thesis_id,)).fetchone()
    if row is None:
        raise Refused(NOT_FOUND, f'unknown thesis {thesis_id!r}; read the theses of '
                                 'plugin_invest_portfolio_status or plugin_invest_instrument_status',
                      'unknown_thesis', thesis_id=thesis_id)
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
    thesis_id, symbol = args['thesis_id'], args['symbol']
    content = {k: args[k] for k in CONTENT}
    old = db.execute('SELECT * FROM theses WHERE id=?', (thesis_id,)).fetchone()
    if old is not None:
        old = row_of(old)
        if old['symbol'] == symbol and {k: old[k] for k in CONTENT} == content:
            if old['retired_at'] is None:
                return  # a replay: assessments never change the portfolio's fields
            raise Refused(CONFLICT, f'thesis {thesis_id} was retired; choose a new thesis_id', 'thesis_retired')
        raise Refused(CONFLICT, f'thesis_id {thesis_id} already names another thesis; choose a new thesis_id',
                      'thesis_id_taken')
    instrument = instruments.get(db, symbol)
    if instrument is None or instrument['state'] != 'live':
        if instrument is None:
            raise Refused(NOT_FOUND, f'{symbol} is not a covered instrument; add it with '
                                     'plugin_invest_instrument_add', 'unknown_instrument', symbol=symbol)
        raise Refused(CONFLICT, f"{symbol} is {instrument['state']}; a thesis needs a live instrument",
                      'instrument_not_live', symbol=symbol, state=instrument['state'])
    if len(open_theses(db, symbol)) >= OPEN_CAP:
        raise Refused(CONFLICT, f'{symbol} has at most {OPEN_CAP} open theses; retire one with '
                                'plugin_invest_thesis_rm first', 'open_thesis_cap', symbol=symbol, cap=OPEN_CAP)
    db.execute('INSERT INTO theses(id,symbol,assessment,version,retired_at,body) VALUES (?,?,?,?,?,?)',
               (thesis_id, symbol, 'open', 1, None, encoded(content | {'added_at': now.isoformat()})))
    ledger.event(db, 'thesis_added', {'thesis_id': thesis_id, 'symbol': symbol,
                                      **{k: v for k, v in content.items() if k != 'body'}, **audit})


def assess(ledger, db, thesis, args, now, **audit):
    """The research assessment of one open thesis, under `expected_version`."""
    if thesis['retired_at'] is not None:
        raise Refused(CONFLICT, f"thesis {thesis['thesis_id']} is retired; assess an open thesis", 'thesis_retired')
    version(args['expected_version'], thesis['version'])
    research = {'summary': args['summary'], 'source_refs': args['source_refs'], 'assessed_at': now.isoformat()}
    body = {k: thesis[k] for k in CONTENT + ('added_at',)} | {'research': research}
    db.execute('UPDATE theses SET assessment=?, version=version+1, body=? WHERE id=?',
               (args['assessment'], encoded(body), thesis['thesis_id']))
    ledger.event(db, 'thesis_assessed', {'thesis_id': thesis['thesis_id'], 'assessment': args['assessment'],
                                         **research, **audit})


def retire(ledger, db, thesis_id, now, message, **audit):
    db.execute('UPDATE theses SET retired_at=?, version=version+1 WHERE id=? AND retired_at IS NULL',
               (now.isoformat(), thesis_id))
    ledger.event(db, 'thesis_retired', {'thesis_id': thesis_id, 'message': message, **audit})


def remove(ledger, db, args, now, **audit):
    """`thesis_rm`: retire one open thesis under `expected_version`."""
    thesis = get(db, args['thesis_id'])
    if thesis['retired_at'] is not None:
        raise Refused(CONFLICT, f"thesis {thesis['thesis_id']} is already retired", 'thesis_retired')
    version(args['expected_version'], thesis['version'])
    retire(ledger, db, thesis['thesis_id'], now, args['message'], **audit)


def retire_open(ledger, db, symbol, now, message, **audit):
    """Retire every open thesis of `symbol`: its instrument is being removed."""
    for thesis in open_theses(db, symbol):
        retire(ledger, db, thesis['thesis_id'], now, message, **audit)
