"""Planner decisions: weight bounds, the held limit and the portfolio Track fence."""
import pytest

from invest import instruments
from rig import OWNER, PLANNER, WORKER, NOW, SOURCES, track


def test_weights_respect_bounds(rig):
    r = rig
    r.configure(max_weight_bps=4000)
    for code in ('AAA', 'BBB', 'DDD'):
        r.quote(f'US:{code}', '100')
    r.watch('US:AAA', 'US:BBB', 'US:DDD')
    with r.app.ledger.session() as db:
        instruments.admit(r.app.ledger, db, 'US:CCC', NOW)  # added, not yet verified
    refused = [
        ({'US:AAA': 4001}, 'max_weight_bps'),
        ({'US:AAA': 4000, 'US:BBB': 4000, 'US:DDD': 1801}, 'cash_buffer_bps'),
        ({'US:CCC': 100}, 'live'),
        ({'US:ZZZ': 100}, 'not a covered instrument'),
        ({'HK:700': 100}, 'US'),
        ({'US:AAA': -1}, 'integer'),
        ({'US:AAA': 1.5}, 'integer'),
        ({'US:AAA': True}, 'integer'),
    ]
    for weights, match in refused:
        with pytest.raises(ValueError, match=match):
            r.decide(weights)
    duplicate = [{'symbol': 'US:AAA', 'bps': 100}, {'symbol': 'us:aaa', 'bps': 200}]
    with pytest.raises(ValueError, match='more than once'):
        r.decide({}, weights=duplicate)
    with pytest.raises(ValueError, match='missing or unknown'):
        r.decide({}, weights=[{'symbol': 'US:AAA'}])
    assert r.status()['decisions'] == []
    # At the bounds, with a lower-case symbol canonicalized.
    state = r.decide({'us:aaa': 4000, 'US:BBB': 4000, 'US:DDD': 1800})
    assert state['decisions'][0]['body']['weights'] == {'US:AAA': 4000, 'US:BBB': 4000, 'US:DDD': 1800}
    assert state['decisions'][0]['state'] == 'queued'


def test_held_after_counts_positions(rig):
    r = rig
    for code in ('AAA', 'BBB', 'CCC'):
        r.quote(f'US:{code}', '100')
    r.account(cash_usd='8000', available_cash_usd='8000')
    r.opening(AAA=10, BBB=10)
    r.configure(max_held=2)
    r.watch('US:CCC')
    # Both positions remain held until their sells settle, so a third symbol cannot be bought yet.
    for weights in ({'US:CCC': 3000}, {'US:AAA': 0, 'US:BBB': 0, 'US:CCC': 3000}):
        with pytest.raises(ValueError, match='max_held'):
            r.decide(weights)
    assert r.status()['decisions'] == []
    # Rotating a full book takes two decisions: sell first ...
    r.decide({}, decision_id='sell')
    r.execute('sell')
    assert [(o['symbol'], o['side']) for o in r.submits()] == [('AAA.US', 'Sell')]
    r.publish(); r.fill()
    assert [(o['symbol'], o['side']) for o in r.submits()][1:] == [('BBB.US', 'Sell')]
    r.publish()
    state = r.fill()
    assert state['decisions'][0]['state'] == 'done' and state['snapshot']['positions'] == {}
    # ... then buy once the sells settled.
    assert r.decide({'US:CCC': 3000}, decision_id='buy')['decisions'][-1]['state'] == 'queued'


def test_research_track_cannot_trade(rig):
    r = rig
    r.quote('US:AAA', '100')
    r.watch('US:AAA')
    r.decide({'US:AAA': 5000})
    before = r.status()
    other = {'decision_id': 'research-plan', 'weights': [{'symbol': 'US:AAA', 'bps': 9000}],
             'message': 'A research Track must never choose the portfolio weights.',
             'source_refs': SOURCES, 'valid_until': before['decisions'][0]['body']['valid_until']}
    for name, args, caller in (('decision_add', other, PLANNER),
                               ('execution_add', {'decision_id': 'd-1'}, WORKER),
                               ('portfolio_status', {}, PLANNER)):
        with pytest.raises(ValueError, match='portfolio Track'):
            r.app.call(track('research-aaa', 'owner', 'invest-US-AAA-1'), name, args, caller)
    assert r.status() == before
    r.step()
    assert r.submits() == []
    assert r.status()['decisions'][0]['state'] == 'queued'


@pytest.mark.parametrize('name,caller,match', [
    ('decision_add', WORKER, 'Planner'), ('execution_add', PLANNER, 'Worker')])
def test_roles_are_fenced(rig, name, caller, match):
    r = rig
    r.quote('US:AAA', '100'); r.watch('US:AAA'); r.decide({'US:AAA': 5000})
    args = {'decision_id': 'd-1'} if name == 'execution_add' else {}
    before = r.status()
    with pytest.raises(ValueError, match=match):
        r.app.call(OWNER, name, args, caller)
    assert r.status() == before


@pytest.mark.parametrize('caller', [None, {}, {'role': 'worker'}, PLANNER | {'card_id': ''},
                                    {'role': 'assistant', 'card_id': 'a', 'session_id': 'b'}])
def test_host_identity_is_required(rig, caller):
    with pytest.raises(ValueError, match='identity'):
        rig.app.call(OWNER, 'portfolio_status', {}, caller)


def test_decision_is_immutable_and_one_is_unresolved(rig):
    r = rig
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    r.decide({'US:AAA': 5000}); r.decide({'US:AAA': 5000})
    with pytest.raises(ValueError, match='different decision'):
        r.decide({'US:AAA': 4000})
    with pytest.raises(ValueError, match='resolve'):
        r.decide({'US:AAA': 4000}, decision_id='d-2')
    assert len(r.status()['decisions']) == 1


def test_refusals_name_the_tool(rig):
    with pytest.raises(ValueError, match='^decision_add: '):
        rig.decide({'US:ZZZ': 100})
