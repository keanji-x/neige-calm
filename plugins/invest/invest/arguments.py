"""The one argument boundary of every invest tool (#2104 §3.6): each argument's type and pattern are
checked here, before any lookup or SQL, so only strings and integers of the declared shape reach the
ledger. A failure is an invalid argument (-32602) and changes nothing."""
import re

from .config import captured, exact, text, timestamp
from .symbols import canonical

DECISION_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,54}')  # also names the Worker task key inv-exec-<id>
THESIS_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,63}')
STANCES = ('bullish', 'bearish', 'neutral')
ASSESSMENTS = ('open', 'holding', 'at_risk', 'broken')
MESSAGE = 2000  # characters of an audit note


def slug(regex, field, shape):
    def check(value):
        if not isinstance(value, str) or not regex.fullmatch(value):
            raise ValueError(f'{field} must be {shape}')
        return value
    return check


def one_of(field, values):
    def check(value):
        if not isinstance(value, str) or value not in values:
            raise ValueError(f'{field} must be one of {", ".join(values)}')
        return value
    return check


def bounded(field, limit, low=1):
    def check(value):
        text(value, field, limit)
        if len(value) < low:
            raise ValueError(f'{field} must contain {low}-{limit} characters')
        return value
    return check


def expected_version(value):
    if type(value) is not int or value < 1:
        raise ValueError('expected_version must be a positive integer')
    return value


def weights(raw):
    """`[{symbol, bps}]` → `{VENUE:CODE: bps}`; shape only, state is checked separately."""
    if not isinstance(raw, list) or len(raw) > 255:
        raise ValueError('weights must be a list of at most 255 {symbol, bps} entries')
    parsed = {}
    for entry in raw:
        exact(entry, {'symbol', 'bps'})
        symbol = canonical(entry['symbol'])
        if type(entry['bps']) is not int or not 0 <= entry['bps'] <= 10000:
            raise ValueError(f'bps of {symbol} must be an integer between 0 and 10000')
        if symbol in parsed:
            raise ValueError(f'{symbol} is weighted more than once')
        parsed[symbol] = entry['bps']
    return dict(sorted(parsed.items()))


def instant(value):
    timestamp(value)
    return value


decision_id = slug(DECISION_ID, 'decision_id', '1-55 lowercase letters, digits or hyphens')
thesis_id = slug(THESIS_ID, 'thesis_id', '1-64 lowercase letters, digits or hyphens, not starting with a hyphen')
message = bounded('message', MESSAGE)
SYMBOL = {'symbol': canonical, 'message': message}
SCHEMAS = {
    'portfolio_status': {},
    'instrument_status': {},
    'decision_add': {'decision_id': decision_id, 'weights': weights, 'message': bounded('message', 6000, 10),
                     'source_refs': captured, 'valid_until': instant},
    'execution_add': {'decision_id': decision_id},
    'instrument_add': SYMBOL,
    'instrument_set': SYMBOL | {'expected_version': expected_version},
    'instrument_rm': SYMBOL | {'expected_version': expected_version},
    'thesis_add': {'thesis_id': thesis_id, 'symbol': canonical, 'stance': one_of('stance', STANCES),
                   'title': bounded('title', 110), 'summary': bounded('summary', 500),
                   'body': bounded('body', 6000), 'source_refs': captured},
    'thesis_set': {'thesis_id': thesis_id, 'assessment': one_of('assessment', ASSESSMENTS),
                   'summary': bounded('summary', 500), 'source_refs': captured,
                   'expected_version': expected_version},
    'thesis_rm': {'thesis_id': thesis_id, 'expected_version': expected_version, 'message': message},
}


def parse(name, args):
    """The checked arguments of tool `name`: every key required, no other key, each of its shape."""
    schema = SCHEMAS[name]
    if not isinstance(args, dict):
        raise ValueError('arguments must be an object')
    exact(args, set(schema))
    return {key: check(args[key]) for key, check in schema.items()}
