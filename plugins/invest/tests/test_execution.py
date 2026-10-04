"""Execution legs, reconciliation and the opening account, through the production loop."""
from datetime import timedelta
import re

import pytest

from rig import NOW


def test_sells_before_buys_settled_cash_only(rig):
    r = rig
    r.quote('US:AAA', '100'); r.quote('US:BBB', '100')
    r.account(cash_usd='200', available_cash_usd='200')
    r.opening(AAA=98)
    r.watch('US:BBB')
    r.decide({'US:BBB': 5000})
    state = r.execute()
    # The sell leg goes first; nothing else is submitted while it is in flight.
    assert [(o['symbol'], o['side'], o['quantity']) for o in r.submits()] == [('AAA.US', 'Sell', 98)]
    assert state['decisions'][0]['state'] == 'working'
    r.step()
    assert len(r.submits()) == 1
    # The sale settles at the broker but its proceeds are not yet settled cash.
    r.publish()
    state = r.fill(settled=False)
    assert state['snapshot']['cash_usd'] == '10000.00' and len(r.submits()) == 1
    assert state['decisions'][0]['state'] == 'working'
    assert 'settled cash' in state['decisions'][0]['error']
    # Settled cash, less the cash reserve and the 1% price reserve, funds the buy: (2020 - 200) / 101.
    r.account(available_cash_usd='2020')
    r.step()
    buy = r.submits()[-1]
    assert (buy['symbol'], buy['side'], buy['quantity']) == ('BBB.US', 'Buy', 18)


def test_unfunded_buy_ends_at_valid_until(rig):
    r = rig
    r.quote('US:BBB', '100')
    r.account(available_cash_usd='0')
    r.watch('US:BBB')
    r.decide({'US:BBB': 5000})
    state = r.execute()
    assert state['decisions'][0]['state'] == 'requested' and 'settled cash' in state['decisions'][0]['error']
    r.clock = lambda: NOW + timedelta(hours=2)
    assert r.step()['decisions'][0]['state'] == 'expired'
    assert r.submits() == []


def test_leg_remarks_are_unique(rig):
    r = rig
    r.quote('US:AAA', '100'); r.quote('US:BBB', '100')
    r.account(cash_usd='5000', available_cash_usd='5000')
    r.opening(AAA=50)
    r.watch('US:BBB')
    r.decide({'US:BBB': 5000})
    r.execute(); r.publish(); r.fill()
    r.publish(); assert r.fill()['decisions'][0]['state'] == 'done'
    r.decide({'US:AAA': 2000}, decision_id='d-2')
    r.execute('d-2'); r.publish(); r.fill()
    r.publish(); assert r.fill()['decisions'][1]['state'] == 'done'
    legs = r.submits()
    assert [(o['symbol'], o['side']) for o in legs] == [
        ('AAA.US', 'Sell'), ('BBB.US', 'Buy'), ('BBB.US', 'Sell'), ('AAA.US', 'Buy')]
    remarks = [o['remark'] for o in legs]
    ids = [o['client_request_id'] for o in legs]
    # One identity per (account, decision, symbol): distinct across symbols and across decisions.
    assert len(set(remarks)) == len(set(ids)) == 4
    for remark, key in zip(remarks, ids):
        assert re.fullmatch(r'nc-inv-[0-9a-f]{32}', remark) and len(remark) == 39
        assert re.fullmatch(r'[0-9a-f]{64}', key) and remark[7:] == key[:32]
    orders = [o for d in r.status()['decisions'] for o in d['orders']]
    assert sorted(o['id'] for o in orders) == sorted(remarks)


def test_unowned_active_order_blocks(rig):
    r = rig
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    r.decide({'US:AAA': 5000})
    before = r.status()['snapshot']
    state = r.read()
    state['snapshot']['orders'].append({
        'order_id': 'manual-1', 'symbol': 'QQQ.US', 'side': 'Buy', 'order_type': 'LO', 'quantity': '1',
        'executed_quantity': '0', 'status': 'New', 'remark': '', 'time_in_force': 'Day',
        'outside_rth': 'RTH_ONLY'})
    r.write(state)
    state = r.execute()
    assert 'unowned active broker order' in state['error']
    assert state['snapshot'] == before and state['decisions'][0]['state'] == 'requested'
    assert r.submits() == []
    state = r.read(); state['snapshot']['orders'][0]['status'] = 'Canceled'; r.write(state)
    state = r.step()
    assert state['error'] is None and [o['symbol'] for o in r.submits()] == ['AAA.US']


def test_opening_positions_pin_first_reconciliation(rig):
    r = rig
    r.quote('US:SPY', '100')
    r.opening(SPY=13)
    state = r.read(); state['snapshot']['positions']['SPY.US'] = {'shares': 12, 'available_shares': 12}; r.write(state)
    state = r.step()
    # A mismatch adopts nothing and pins nothing.
    assert 'holdings disagree' in state['error'] and state['snapshot'] is None
    assert state['opening_positions'] is None and state['valuations'] == []
    r.opening(SPY=12)
    state = r.step()
    assert state['error'] is None and state['opening_positions'] == {'US:SPY': 12}
    assert {k: v['shares'] for k, v in state['snapshot']['positions'].items()} == {'US:SPY': 12}
    assert [(i['symbol'], i['state'], i['held']) for i in state['instruments']] == [('US:SPY', 'live', True)]
    # Pinned: the acknowledgement cannot change on this ledger.
    r.opening(SPY=13)
    after = r.step()
    assert 'opening_positions cannot change' in after['error']
    assert after['snapshot'] == state['snapshot'] and after['valuations'] == state['valuations']


def test_cutover_refuses_unquiesced_account(rig):
    r = rig
    r.quote('US:SPY', '100')
    r.opening(SPY=100)
    state = r.read()
    state['snapshot']['orders'].append({
        'order_id': 'paper-1', 'symbol': 'SPY.US', 'side': 'Sell', 'order_type': 'MO', 'quantity': '10',
        'executed_quantity': '0', 'status': 'New', 'remark': 'nc-spy-' + 'a' * 32, 'time_in_force': 'Day',
        'outside_rth': 'RTH_ONLY'})
    r.write(state)
    state = r.step()
    assert 'unowned active broker order' in state['error']
    assert state['snapshot'] is None and state['opening_positions'] is None and state['valuations'] == []
    state = r.read(); state['snapshot']['orders'][0]['status'] = 'Canceled'; r.write(state)
    state = r.step()
    assert state['error'] is None and state['opening_positions'] == {'US:SPY': 100}
    assert {k: v['shares'] for k, v in state['snapshot']['positions'].items()} == {'US:SPY': 100}


def test_external_position_blocks_execution(rig):
    r = rig
    r.quote('US:AAA', '100'); r.quote('US:QQQ', '100'); r.watch('US:AAA')
    r.decide({'US:AAA': 5000})
    state = r.read(); state['snapshot']['positions']['QQQ.US'] = {'shares': 1, 'available_shares': 1}; r.write(state)
    state = r.execute()
    assert 'holdings disagree' in state['error'] and r.submits() == []


def test_intent_is_durable_before_broker_write(rig):
    r = rig
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    r.decide({'US:AAA': 6000}); r.execute()
    [call] = [c for c in r.calls() if c['method'] == 'submit']
    assert call['persisted_state'] == 'submitting' and call['persisted_request'] == call['request']
    assert call['request']['quantity'] == 60


def test_lost_response_is_never_resubmitted(rig):
    r = rig
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    r.decide({'US:AAA': 6000})
    state = r.read(); state['fail'] = True; r.write(state)
    state = r.execute()
    assert [o['state'] for o in state['decisions'][0]['orders']] == ['unknown']
    assert r.step()['decisions'][0]['state'] == 'working'
    # The broker did accept it: recovery is by the leg's remark, never by a second submission.
    state = r.read(); del state['fail']; r.write(state)
    r.publish()
    state = r.fill()
    assert [o['state'] for o in state['decisions'][0]['orders']] == ['settled']
    assert state['decisions'][0]['state'] == 'done' and len(r.submits()) == 1


def test_restart_marks_a_crashed_submission_unknown(rig, monkeypatch):
    r = rig
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    r.decide({'US:AAA': 6000}); r.request()

    class Crash(BaseException):
        pass

    def crash(request):
        raise Crash()
    monkeypatch.setattr(r.broker, 'submit', crash)
    try:
        r.step()
    except Crash:
        pass
    monkeypatch.undo()
    r.restart()
    assert [o['state'] for o in r.status()['decisions'][0]['orders']] == ['unknown']
    for _ in range(2):
        r.step()
    assert r.submits() == []


def test_waits_for_the_regular_session_and_expires(rig):
    r = rig
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    r.account(market_open=False)
    r.decide({'US:AAA': 5000})
    decision = r.execute()['decisions'][0]
    assert decision['state'] == 'requested' and 'regular session' in decision['error']
    r.clock = lambda: NOW + timedelta(hours=2)
    assert r.step()['decisions'][0]['state'] == 'expired' and r.submits() == []


def test_unverified_symbol_is_dropped(rig):
    r = rig
    state = r.watch('US:NOPE')
    assert [(i['symbol'], i['state']) for i in state['instruments']] == [('US:NOPE', 'dropped')]
    assert state['error'] is None


def test_transport_environment_allowlist(rig, monkeypatch):
    monkeypatch.setenv('LONGBRIDGE_HTTP_URL', 'https://untrusted.example')
    monkeypatch.setenv('MODEL_API_KEY', 'fixture-secret')
    rig.step()
    keys = rig.calls()[0]['env_keys']
    assert 'LONGBRIDGE_HTTP_URL' not in keys and 'MODEL_API_KEY' not in keys


def test_restart_never_readmits_a_pinned_opening_symbol(rig):
    r = rig
    r.quote('US:SPY', '100')
    r.opening(SPY=13)
    assert r.step()['opening_positions'] == {'US:SPY': 13}
    with r.app.ledger.session() as db:  # a later slice's removal, applied directly
        db.execute("UPDATE instruments SET state='dropped' WHERE symbol='US:SPY'")
    r.restart()
    assert [(i['symbol'], i['state']) for i in r.status()['instruments']] == [('US:SPY', 'dropped')]


def test_order_is_capped_at_max_order_bps(rig):
    r = rig
    r.configure(max_order_bps=500)
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    r.decide({'US:AAA': 6000}); r.execute()
    # 5% of 10000 with the 1% price reserve: floor(500 / 101) shares.
    assert [o['quantity'] for o in r.submits()] == [4]


def test_drift_band_leaves_a_close_weight_untraded(rig):
    r = rig
    r.configure(drift_bps=100)
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    r.decide({'US:AAA': 90})
    assert r.execute()['decisions'][0]['state'] == 'noop' and r.submits() == []


@pytest.mark.parametrize('quote,reason', [
    ({'at': NOW - timedelta(minutes=5)}, 'stale'), ({'status': 'Halted'}, 'trading status is Halted')])
def test_stale_or_halted_quote_waits(rig, quote, reason):
    r = rig
    r.quote('US:AAA', '100', at=quote.get('at'), status=quote.get('status', 'Normal'))
    r.watch('US:AAA')
    r.decide({'US:AAA': 5000})
    decision = r.execute()['decisions'][0]
    assert decision['state'] == 'requested' and reason in decision['error'] and r.submits() == []
    r.quote('US:AAA', '100')
    r.step()
    assert [o['symbol'] for o in r.submits()] == ['AAA.US']
