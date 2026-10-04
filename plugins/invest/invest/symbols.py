"""US instrument symbols: canonical `VENUE:CODE` (upper case), the SDK's `CODE.VENUE`, and unit ids
`VENUE.CODE`. The venue never contains `.`, so every mapping is injective."""
import re

SYMBOL = re.compile(r'([A-Za-z]{2,8}):([A-Za-z0-9._-]{1,32})')
VENUE = 'US'


def canonical(value):
    match = SYMBOL.fullmatch(value) if isinstance(value, str) else None
    if match is None:
        raise ValueError(f'symbol {value!r} must be VENUE:CODE, for example US:SPY')
    venue, code = match.group(1).upper(), match.group(2).upper()
    if venue != VENUE:
        raise ValueError(f'symbol {value!r}: only US instruments are supported')
    return f'{venue}:{code}'


def to_sdk(symbol):
    venue, code = symbol.split(':', 1)
    return f'{code}.{venue}'


def from_sdk(value):
    if not isinstance(value, str) or '.' not in value:
        raise ValueError('broker symbol must be CODE.US')
    code, venue = value.rsplit('.', 1)
    return canonical(f'{venue}:{code}')


def unit_id(symbol):
    return symbol.replace(':', '.', 1)
