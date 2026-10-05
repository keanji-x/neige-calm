"""The one argument boundary: every id-like argument is type- and pattern-checked before any lookup or
SQL, so a wrong-typed value is an invalid argument (-32602) and changes nothing."""
from datetime import timedelta

import pytest

from invest.errors import INVALID
from rig import NOW, OWNER, PLANNER, SOURCES, WORKER, refusal, track

BAD = {'int': 1, 'float': 1.5, 'bool': True, 'null': None, 'array': ['aaa-1'], 'object': {'id': 'aaa-1'}}
BAD_ID = BAD | {'pattern': 'AAA 1'}            # thesis_id, decision_id
BAD_SYMBOL = BAD | {'pattern': 'US SPY'}       # symbol
BAD_VERSION = {k: v for k, v in BAD.items() if k != 'int'} | {'string': '1', 'zero': 0}


def setup(r):
    """AAA live with one open thesis and a queued decision; BBB live and watched."""
    r.cover('US:AAA', 'US:BBB')
    r.thesis('aaa-1', 'US:AAA')
    r.decide({'US:AAA': 1000})
    return r.instrument('US:AAA'), r.open_thesis('aaa-1')


def calls(r):
    """`tool -> (track, caller, valid arguments)` for every tool that takes arguments."""
    instrument, thesis = r.instrument('US:AAA'), r.open_thesis('aaa-1')
    research = track(f"research-{instrument['key']}", 'owner', instrument['key'])
    note = {'message': 'Audit note for this change.'}
    return {
        'decision_add': (OWNER, PLANNER, {  # a replay of the queued d-1
            'decision_id': 'd-1', 'weights': [{'symbol': 'US:AAA', 'bps': 1000}],
            'message': 'Captured research and market evidence support these weights.', 'source_refs': SOURCES,
            'valid_until': (NOW + timedelta(hours=1)).isoformat()}),
        'execution_add': (OWNER, WORKER, {'decision_id': 'd-1'}),
        'instrument_add': (OWNER, PLANNER, {'symbol': 'US:CCC'} | note),
        'instrument_set': (OWNER, PLANNER, {'symbol': 'US:AAA', 'expected_version': instrument['version']} | note),
        'instrument_rm': (OWNER, PLANNER, {'symbol': 'US:BBB',
                                           'expected_version': r.instrument('US:BBB')['version']} | note),
        'thesis_add': (OWNER, PLANNER, {'thesis_id': 'aaa-2', 'symbol': 'US:AAA', 'stance': 'bearish',
                                        'title': 'A second thesis', 'summary': 'Summary.', 'body': 'Body.',
                                        'source_refs': SOURCES}),
        'thesis_set': (research, PLANNER, {'thesis_id': 'aaa-1', 'assessment': 'holding', 'summary': 'Holds.',
                                           'source_refs': SOURCES, 'expected_version': thesis['version']}),
        'thesis_rm': (OWNER, PLANNER, {'thesis_id': 'aaa-1', 'expected_version': thesis['version']} | note),
    }


CASES = [(tool, arg, kind, value)
         for tool, args in (('decision_add', ('decision_id',)), ('execution_add', ('decision_id',)),
                            ('instrument_add', ('symbol',)), ('instrument_set', ('symbol', 'expected_version')),
                            ('instrument_rm', ('symbol', 'expected_version')), ('thesis_add', ('thesis_id', 'symbol')),
                            ('thesis_set', ('thesis_id', 'expected_version')),
                            ('thesis_rm', ('thesis_id', 'expected_version')))
         for arg in args
         for kind, value in {'symbol': BAD_SYMBOL, 'expected_version': BAD_VERSION}.get(arg, BAD_ID).items()]


@pytest.mark.parametrize('tool,arg,kind,value', CASES, ids=[f'{t}-{a}-{k}' for t, a, k, _ in CASES])
def test_id_arguments_are_checked_before_the_ledger(rig, tool, arg, kind, value):
    r = rig
    setup(r)
    meta, caller, args = calls(r)[tool]
    before = r.dump()
    with refusal(INVALID, f'^plugin_invest_{tool}: ') as caught:
        r.app.call(meta, tool, args | {arg: value}, caller)
    assert caught.value.data == {'refusal': 'invalid_argument'}
    assert r.dump() == before
    r.app.call(meta, tool, args, caller)  # the same call with the valid value is admitted


@pytest.mark.parametrize('tool', ['decision_add', 'execution_add', 'instrument_add', 'thesis_set', 'thesis_rm'])
@pytest.mark.parametrize('args', [[], 'aaa-1', None], ids=['array', 'string', 'null'])
def test_arguments_must_be_an_object(rig, tool, args):
    r = rig
    setup(r)
    meta, caller, _ = calls(r)[tool]
    before = r.dump()
    with refusal(INVALID, 'arguments must be an object'):
        r.app.call(meta, tool, args, caller)
    assert r.dump() == before


def test_thesis_id_never_reaches_sqlite_coerced(rig):
    """Thesis "1" exists: the integer 1 must not name it through SQLite's type affinity."""
    r = rig
    setup(r)
    r.thesis('1', 'US:AAA')
    meta, caller, _ = calls(r)['thesis_set']
    version = r.open_thesis('1')['version']
    before = r.dump()
    for value in (1, ['1'], {'id': '1'}, None):
        with refusal(INVALID, 'thesis_id'):
            r.app.call(OWNER, 'thesis_rm', {'thesis_id': value, 'expected_version': version,
                                            'message': 'Retire thesis one.'}, PLANNER)
        with refusal(INVALID, 'thesis_id'):
            r.app.call(meta, 'thesis_set', {'thesis_id': value, 'assessment': 'broken', 'summary': 'No.',
                                            'source_refs': SOURCES, 'expected_version': version}, caller)
    assert r.dump() == before
