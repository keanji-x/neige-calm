"""The operator's typed configuration: broker binding, the portfolio Track, limits and trading policy."""
from dataclasses import dataclass
import json
from pathlib import Path

from .config import exact, identifier
from .symbols import canonical

REQUIRED = frozenset(('account_no', 'broker_home', 'portfolio_track_id', 'oauth_client_id', 'sdk_python_path',
                      'max_held', 'max_watched', 'max_weight_bps'))
OPTIONAL = frozenset(('access_region', 'poll_seconds', 'cash_buffer_bps', 'drift_bps', 'max_order_bps',
                      'quote_max_age_seconds', 'opening_positions'))
# The kernel's open-Track cap is 1..=256; one slot stays free for a renewal (#2104 §3.4).
MAX_COVERED = 255


def opening_positions(value):
    """`[{"symbol": "US:SPY", "shares": 13}]` as a JSON string: the kernel's config schema is scalar."""
    try:
        rows = json.loads(value) if isinstance(value, str) else None
    except ValueError:
        rows = None
    if not isinstance(rows, list):
        raise ValueError('opening_positions must be a JSON array of {"symbol", "shares"} objects')
    positions = {}
    for row in rows:
        exact(row, {'symbol', 'shares'})
        symbol = canonical(row['symbol'])
        if type(row['shares']) is not int or not 1 <= row['shares'] <= 100_000_000:
            raise ValueError(f'opening_positions: shares of {symbol} must be a positive integer')
        if symbol in positions:
            raise ValueError(f'opening_positions: {symbol} is listed twice')
        positions[symbol] = row['shares']
    return tuple(sorted(positions.items()))


@dataclass(frozen=True)
class InvestConfig:
    account_no: str
    broker_home: str
    portfolio_track_id: str
    oauth_client_id: str
    sdk_python_path: str
    max_held: int
    max_watched: int
    max_weight_bps: int
    access_region: str = 'global'
    poll_seconds: int = 60
    cash_buffer_bps: int = 200
    drift_bps: int = 100
    max_order_bps: int = 1000
    quote_max_age_seconds: int = 60
    opening_positions: tuple = ()

    @classmethod
    def parse(cls, values):
        exact(values, REQUIRED, OPTIONAL)
        values = dict(values)
        values['opening_positions'] = opening_positions(values.get('opening_positions', '[]'))
        obj = cls(**values)
        identifier(obj.account_no)
        identifier(obj.oauth_client_id)
        for field in ('broker_home', 'sdk_python_path'):
            path = getattr(obj, field)
            if not isinstance(path, str) or '\0' in path or not Path(path).is_absolute():
                raise ValueError(f'{field} must be an absolute path')
        if not isinstance(obj.portfolio_track_id, str) or not obj.portfolio_track_id.strip() \
                or len(obj.portfolio_track_id) > 256:
            raise ValueError('portfolio_track_id is required')
        if obj.access_region not in ('global', 'cn'):
            raise ValueError('access_region must be global or cn')
        for field, low, high in (('max_held', 1, MAX_COVERED), ('max_watched', 1, MAX_COVERED),
                                 ('max_weight_bps', 1, 10000), ('poll_seconds', 5, 3600),
                                 ('max_order_bps', 1, 10000), ('cash_buffer_bps', 1, 2000),
                                 ('drift_bps', 0, 1000), ('quote_max_age_seconds', 1, 120)):
            value = getattr(obj, field)
            if type(value) is not int or not low <= value <= high:
                raise ValueError(f'{field} must be an integer in {low}..{high}')
        if obj.max_held + obj.max_watched > MAX_COVERED:
            raise ValueError(f'max_held + max_watched must be at most {MAX_COVERED}')
        if len(obj.opening_positions) > obj.max_held:
            raise ValueError('opening_positions lists more symbols than max_held')
        return obj
