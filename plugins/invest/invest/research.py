"""Research Tracks: write authority from kernel provenance, and the view of one symbol (#2104 §3.3).

A call is attested for symbol S exactly when the kernel's provenance names the portfolio Track as its
Track's creator, its creator key is S's current key, and S is live. Nothing is stored at binding time:
provenance is copied from the Track row on every call. An older key of S, or a dropped S, is
superseded (-32409); every other caller is forbidden (-32403), the portfolio Track included.
"""
from decimal import Decimal

from . import instruments, theses
from .errors import CONFLICT, FORBIDDEN, Refused

PROVENANCE = ('id', 'creator_track_id', 'creator_key')
BPS = Decimal('0.01')


def context(meta):
    """The host's `_meta["dev.neige/track"]`: `{id, creator_track_id, creator_key}`, creator fields null
    when the Track was not added by a Planner. All three are required; any other key is ignored."""
    if not isinstance(meta, dict) or not set(PROVENANCE) <= set(meta) or not isinstance(meta['id'], str) \
            or not meta['id'] or any(meta[k] is not None and not isinstance(meta[k], str) for k in PROVENANCE[1:]):
        raise Refused(FORBIDDEN, 'host-provided Track context {id, creator_track_id, creator_key} required',
                      'track_context')
    return {k: meta[k] for k in PROVENANCE}


def attest(db, config, track):
    """The live instrument whose current key `track` was added under by the portfolio Track."""
    key = track['creator_key']
    if track['creator_track_id'] != config.portfolio_track_id or key is None:
        raise Refused(FORBIDDEN, 'only a research Track that the portfolio Track added may call this tool',
                      'not_attested')
    parsed = instruments.parse_key(key)
    row = instruments.get(db, parsed[0]) if parsed else None
    if row is None or parsed[1] > row['key_seq']:
        raise Refused(FORBIDDEN, f'research key {key} was never issued', 'not_attested')
    if row['state'] != 'live' or parsed[1] != row['key_seq']:
        current = row['body']['track_add']['idempotency_key'] if row['state'] == 'live' else None
        raise Refused(CONFLICT, f'superseded: close this Track with neige_track_close; {row["symbol"]} is '
                                f'{row["state"]} and its current key is {current or "none"}, not {key}',
                      'superseded', symbol=row['symbol'], key=current)
    return row


def view(db, row, snapshot, targets, held, portfolio_track_id):
    """`instrument_status`: this Track's own symbol only, never the rest of the portfolio."""
    symbol = row['symbol']
    position = None
    if snapshot is not None:
        held_now = snapshot['positions'].get(symbol)
        equity = Decimal(snapshot['equity_usd'])
        value = Decimal(held_now['value_usd']) if held_now else Decimal(0)
        position = {'shares': held_now['shares'] if held_now else 0,
                    'price': snapshot['quotes'].get(symbol, {}).get('price'),
                    'value_usd': str(value),
                    'weight_bps': str((value / equity * 10000).quantize(BPS)) if equity > 0 else None}
    return {'symbol': symbol, 'state': row['state'], 'key': row['body']['track_add']['idempotency_key'],
            'portfolio_track_id': portfolio_track_id,
            'held': symbol in held, 'target_bps': targets.get(symbol, 0),
            'snapshot': {'at': snapshot['at'], 'date': snapshot['date']} if snapshot else None,
            'position': position,
            'theses': theses.open_theses(db, symbol) + theses.retired_theses(db, symbol)}
