"""Theses in the plugin ledger, the coverage limits, and the board and research units (#2104 §3.2, §3.7)."""
import json
import re

import pytest

from account import SimulatedAccount
from invest import theses
from invest.research_views import research_units
from invest.symbols import canonical, unit_id
from invest.views import units
from recipe import unit_kinds
from rig import NOW, SOURCES
from test_projections import valid

ID = re.compile(r'[A-Za-z0-9._-]{1,100}')


def board(state):
    [dataset] = units(state)['thesis.board']['cell']['datasets']
    return dataset['items']


def test_board_shows_assessment_change(rig):
    r = rig
    r.cover('US:AAA')
    r.thesis('aaa-margins', 'US:AAA', title='Margins keep expanding')
    current = r.instrument('US:AAA')['key']
    r.research(current, 'instrument_status')  # seen: the research Track is bound
    before = board(r.step())
    [record] = before
    assert record['id'] == 'US.AAA'
    [section] = record['sections']
    assert section == {'label': '待评估 · Margins keep expanding', 'body': 'Demand outruns supply through next year.'}
    # Only the assessment changes: the same summary and sources.
    r.assess(current, 'aaa-margins', 'at_risk')
    after = board(r.step())
    assert after[0]['sections'] == [{'label': '承压 · Margins keep expanding', 'body': section['body']}]
    assert after[0] | {'sections': []} == before[0] | {'sections': []}


def test_fourth_open_thesis_refused_state_unchanged(rig):
    r = rig
    r.cover('US:AAA', 'US:BBB')
    for n in (1, 2, 3):
        r.thesis(f'aaa-{n}', 'US:AAA')
    before = r.dump()
    with pytest.raises(ValueError, match='at most 3 open theses'):
        r.thesis('aaa-4', 'US:AAA')
    assert r.dump() == before
    # The cap is per symbol, and a retired thesis frees its place.
    r.thesis('bbb-1', 'US:BBB')
    r.retire('aaa-1')
    r.thesis('aaa-4', 'US:AAA')
    assert sorted(t['thesis_id'] for t in r.status()['theses'] if t['symbol'] == 'US:AAA') == ['aaa-2', 'aaa-3', 'aaa-4']


def test_rm_retires_open_theses(rig):
    r = rig
    r.cover('US:AAA', 'US:BBB')
    r.thesis('aaa-1', 'US:AAA'); r.thesis('aaa-2', 'US:AAA'); r.thesis('bbb-1', 'US:BBB')
    r.retire('aaa-2')
    r.remove('US:AAA')
    state = r.status()
    assert r.instrument('US:AAA')['state'] == 'dropped'
    assert [t['thesis_id'] for t in state['theses']] == ['bbb-1']
    with r.app.ledger.session() as db:
        rows = {row['id']: row['retired_at'] for row in db.execute('SELECT id, retired_at FROM theses')}
    assert rows['aaa-1'] is not None and rows['aaa-2'] is not None and rows['bbb-1'] is None
    assert [record['id'] for record in board(r.step())] == ['US.BBB']
    # Re-adding the symbol issues the next key, and its old theses no longer count.
    r.cover('US:AAA')
    assert r.instrument('US:AAA')['key'] == 'invest-US-AAA-2'
    for n in (3, 4, 5):
        r.thesis(f'aaa-{n}', 'US:AAA')
    # A held symbol cannot be removed.
    r.decide({'US:BBB': 1000})
    with pytest.raises(ValueError, match='held'):
        r.remove('US:BBB')


def test_dropped_counts_toward_no_limit(rig):
    r = rig
    r.configure(max_watched=2)
    r.cover('US:AAA', 'US:BBB')
    with pytest.raises(ValueError, match='max_watched'):
        r.add('US:CCC')
    r.remove('US:AAA')
    r.add('US:CCC')  # pending: counted
    with pytest.raises(ValueError, match='max_watched'):
        r.add('US:DDD')
    state = r.step()  # no quote for CCC: verification drops it
    assert {i['symbol']: i['state'] for i in state['instruments']} == {
        'US:AAA': 'dropped', 'US:BBB': 'live', 'US:CCC': 'dropped'}
    assert state['limits']['watched'] == 1
    r.add('US:DDD')
    assert r.status()['limits']['watched'] == 2


def test_units_fit_caps_at_max_config(rig):
    r = rig
    held = [f'US:H{i:03d}{"X" * 28}' for i in range(128)]
    watched = [f'US:W{i:03d}{"X" * 28}' for i in range(127)]
    prices = {f'{s.split(":")[1]}.US': 10 + i for i, s in enumerate(held + watched)}
    r.configure(max_held=128, max_watched=127, opening_positions=json.dumps(
        [{'symbol': s, 'shares': 10} for s in held]))
    account = SimulatedAccount('1000000', prices, lambda: r.clock())
    account.positions = {f'{s.split(":")[1]}.US': 10 for s in held}
    r.broker = account
    r.restart()
    r.watch(*watched)
    cjk = '研' * 6000
    target = held[0]
    with r.app.ledger.session() as db:
        for n in range(20):  # a long history of retired theses, then three open ones on every symbol
            thesis_id = f'retired-{n}'
            theses.add(r.app.ledger, db, {'thesis_id': thesis_id, 'symbol': target, 'stance': 'bearish',
                                          'title': cjk[:110], 'summary': cjk[:500], 'body': cjk,
                                          'source_refs': SOURCES}, NOW)
            theses.retire(r.app.ledger, db, thesis_id, NOW, 'retired for the fixture')
        for s in held + watched:
            for n in range(3):
                theses.add(r.app.ledger, db, {
                    'thesis_id': f'{unit_id(s).lower().replace(".", "-")[:60]}-{n}', 'symbol': s,
                    'stance': 'neutral', 'title': cjk[:110], 'summary': cjk[:500], 'body': cjk,
                    'source_refs': SOURCES}, NOW)
    state = r.step()
    assert state['error'] is None and len(state['snapshot']['positions']) == 128
    assert sum(i['state'] == 'live' for i in state['instruments']) == 255
    published = units(state)
    assert set(published) == unit_kinds()
    for unit in published.values():
        valid(unit)
    records = published['thesis.board']['cell']['datasets'][0]['items']
    assert len(records) == 100 and records[-1]['id'] == 'other'
    assert all(len(record['sections']) == 3 for record in records[:-1])
    # The research units of the symbol with the longest history: 3 open + the 20 latest retired.
    current = next(i for i in state['instruments'] if i['symbol'] == target)['key']
    view = r.research(current, 'instrument_status')
    research = research_units(view)
    assert set(research) == unit_kinds('instrument-recipe.md')
    for unit in research.values():
        valid(unit)
    assert len(research['thesis.records']['cell']['datasets'][0]['items']) == 23


def test_unit_ids_injective(rig):
    r = rig
    symbols = ['US:A.B', 'US:A-B', 'US:A_B', 'US:AB', 'US:A.B.C', 'US:A..B', 'US:A-.B', 'US:B.A']
    r.configure(max_watched=len(symbols))
    r.cover(*symbols)
    for s in symbols:
        r.thesis(f't-{symbols.index(s)}', s)
    ids = [record['id'] for record in board(r.step())]
    assert sorted(ids) == sorted(unit_id(s) for s in symbols)
    assert len(set(ids)) == len(symbols) and 'other' not in ids
    for identity in ids:
        assert ID.fullmatch(identity)
        venue, code = identity.split('.', 1)  # the venue has no `.`: the id inverts to its symbol
        assert canonical(f'{venue}:{code}') in symbols
