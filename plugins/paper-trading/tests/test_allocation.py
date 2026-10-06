"""Production allocation and subprocess entry points, with prescribed external records."""
from datetime import datetime, timedelta, timezone
from decimal import Decimal
import json
from pathlib import Path
import re
import time

import pytest

from paper_trading.allocation import TOOLS, Allocation
from paper_trading.allocation_broker import AllocationBroker
from paper_trading.allocation_config import OPTIONAL, REQUIRED, AllocationConfig
from paper_trading.allocation_views import units
from .host import Host
from .recipe import unit_kinds

NOW = datetime(2026, 9, 30, 15, tzinfo=timezone.utc)
ROOT = Path(__file__).parents[1]
PLANNER = {'role': 'planner', 'card_id': 'planner-card', 'session_id': 'planner-session'}
WORKER = {'role': 'worker', 'card_id': 'worker-card', 'session_id': 'worker-session'}


@pytest.fixture
def allocation_rig(tmp_path):
    home = tmp_path / 'home'; home.mkdir()
    root = tmp_path / 'data'; root.mkdir()
    values = {'profile': 'spy_cash', 'account_no': 'PAPER123', 'broker_home': str(home),
              'owner_track_id': 'owner', 'oauth_client_id': 'sdk-client',
              'sdk_python_path': str(ROOT / 'tests/allocation_fixture.py'),
              'max_order_bps': 10000, 'poll_seconds': 5, 'drift_bps': 0}
    snapshot = {'identity': {'account_no': 'PAPER123', 'account_channel': 'lb_papertrading'},
                'cash_usd': '10000', 'available_cash_usd': '10000', 'shares': 0, 'available_shares': 0,
                'quote': {'price': '100', 'at': NOW.isoformat(), 'status': 'Normal',
                          'calendar_date': '2026-09-30', 'trading_day': True, 'half_day': False,
                          'regular_close_at': '2026-09-30T20:00:00+00:00'},
                'market_open': True, 'orders': [], 'fills': []}
    path = home / 'allocation-broker.json'; path.write_text(json.dumps({'snapshot': snapshot}))

    class Rig:
        def __init__(self):
            self.home, self.root, self.path, self.values = home, root, path, values
            self.config = AllocationConfig.parse(values)
            self.broker = AllocationBroker(self.config, str(root))
            self.app = Allocation(root, self.config, self.broker, clock=lambda: NOW)

        def restart(self):
            self.app = Allocation(self.root, self.config, self.broker, clock=self.app.clock)

        def read(self):
            return json.loads(path.read_text())

        def write(self, state):
            path.write_text(json.dumps(state))

        def plan(self, **changes):
            args = {'decision_id': 'allocation-1', 'target_spy_bps': 6000,
                    'rationale': 'Captured research and market evidence support this allocation.',
                    'source_refs': ['neige://source/research-1', 'neige://source/market-1'],
                    'valid_until': (NOW + timedelta(hours=1)).isoformat(), **changes}
            return self.app.call('owner', 'spy.plan', args, PLANNER)

        def request(self, key='allocation-1', caller=WORKER):
            return self.app.call('owner', 'spy.execute', {'decision_id': key}, caller)

        def step(self):
            # One background-loop pass: reconcile, then act on a requested decision.
            [(track, state)] = self.app.process_once()
            assert track == 'owner'
            return state

        def execute(self, key='allocation-1'):
            assert self.request(key)['decisions'][-1]['state'] == 'requested'
            return self.step()

        def calls(self):
            p = home / 'allocation-calls.jsonl'
            return [json.loads(l) for l in p.read_text().splitlines()] if p.exists() else []

        def submits(self):
            return [c for c in self.calls() if c['method'] == 'submit']

        def order(self):
            return self.submits()[-1]['request']

        def publish(self, key='order-1', executed=0, status='New'):
            state = self.read(); request = self.order()
            state['snapshot']['orders'].append(request | {'order_id': key, 'quantity': str(request['quantity']),
                                                         'executed_quantity': str(executed), 'status': status})
            self.write(state)

        def fill(self, key, shares, cash, fill_id, qty, status='Filled'):
            state = self.read(); snapshot = state['snapshot']
            o = next(o for o in snapshot['orders'] if o['order_id'] == key)
            o['status'] = status; o['executed_quantity'] = str(int(o['executed_quantity']) + qty)
            snapshot['shares'] = snapshot['available_shares'] = shares
            snapshot['cash_usd'] = snapshot['available_cash_usd'] = cash
            snapshot['fills'].append({'trade_id': fill_id, 'order_id': key, 'symbol': 'SPY.US',
                                      'quantity': str(qty), 'price': '100', 'time': NOW.isoformat()})
            self.write(state)
            return self.step()

        def status(self):
            return self.app.call('owner', 'spy.status', {}, PLANNER)

    return Rig()


def test_spy_buy_sell_and_restart_closed_loop(allocation_rig):
    r = allocation_rig
    r.plan()
    requested = r.request()
    assert requested['decisions'][0]['state'] == 'requested' and not r.calls()
    audit = [e['body'] for e in requested['journal'] if e['kind'] == 'allocation_execution_requested']
    assert audit == [{'decision_id': 'allocation-1', 'caller': WORKER}]
    assert r.step()['decisions'][0]['state'] == 'working'
    assert r.order()['side'] == 'Buy' and r.order()['quantity'] == 60
    assert r.order()['symbol'] == 'SPY.US' and r.order()['order_type'] == 'MO'
    r.publish(); r.fill('order-1', 60, '4000', 'buy-fill', 60)
    assert r.status()['snapshot']['actual_spy_bps'] == '6000.0'
    assert r.status()['decisions'][0]['state'] == 'settled'
    r.plan(decision_id='allocation-2', target_spy_bps=3000)
    state = r.read(); state['response'] = {'order_id': 'order-2'}; r.write(state)
    r.execute('allocation-2')
    assert r.order()['side'] == 'Sell' and r.order()['quantity'] == 30
    r.publish('order-2'); r.fill('order-2', 30, '7000', 'sell-fill', 30)
    r.restart()
    state = r.step()
    assert state['snapshot']['shares'] == 30 and Decimal(state['snapshot']['actual_spy_bps']) == 3000
    assert [d['state'] for d in state['decisions']] == ['settled', 'settled']
    assert len(state['fills']) == 2 and len(r.submits()) == 2


@pytest.mark.parametrize('name,caller,args,match', [
    ('spy.plan', WORKER, None, 'Planner identity'),
    ('spy.execute', PLANNER, {'decision_id': 'allocation-1'}, 'Worker identity'),
], ids=['plan-requires-planner', 'execute-requires-worker'])
def test_spy_role_fence(allocation_rig, name, caller, args, match):
    r = allocation_rig
    if name == 'spy.execute':
        r.plan()
    else:
        args = {'decision_id': 'worker-target', 'target_spy_bps': 6000,
                'rationale': 'A Worker must never choose the allocation target.',
                'source_refs': ['neige://source/research-1'],
                'valid_until': (NOW + timedelta(hours=1)).isoformat()}
    before = r.status()
    with pytest.raises(ValueError, match=match):
        r.app.call('owner', name, args, caller)
    assert r.status() == before
    r.step()
    assert not r.submits()


@pytest.mark.parametrize('caller', [None, {}, {'role': 'worker'}, PLANNER | {'card_id': ''},
                                    {'role': 'assistant', 'card_id': 'a', 'session_id': 'b'}])
def test_spy_missing_caller_fence(allocation_rig, caller):
    with pytest.raises(ValueError, match='identity'):
        allocation_rig.app.call('owner', 'spy.status', {}, caller)


def test_spy_track_fence(allocation_rig):
    r = allocation_rig; r.plan()
    with pytest.raises(ValueError, match='own'):
        r.app.call('other', 'spy.execute', {'decision_id': 'allocation-1'}, WORKER)
    assert r.status()['decisions'][0]['state'] == 'queued'


@pytest.mark.parametrize('change', [{'account_channel': 'lb'}, {'account_no': 'OTHER'}, {'account_channel': None}])
def test_spy_paper_account_fence(allocation_rig, change):
    r = allocation_rig; r.plan()
    state = r.read(); state['snapshot']['identity'].update(change); r.write(state)
    state = r.execute()
    assert 'identity' in state['error'] and state['decisions'][0]['state'] == 'requested'
    assert not r.submits()


def test_spy_target_immutable_and_one_pending_decision(allocation_rig):
    r = allocation_rig; r.plan(); r.plan()
    with pytest.raises(ValueError, match='different target'):
        r.plan(target_spy_bps=5000)
    with pytest.raises(ValueError, match='resolve'):
        r.plan(decision_id='second')
    assert len(r.status()['decisions']) == 1


def test_spy_repeated_request_records_nothing(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['snapshot']['market_open'] = False; r.write(state)
    first = r.request()
    assert r.request() == first
    with pytest.raises(ValueError, match='unknown decision'):
        r.request('never-planned')


def test_spy_no_repeat_submit_after_lost_response(allocation_rig):
    r = allocation_rig; r.plan()
    body = r.status()['decisions'][0]['body']
    with r.app.ledger.session() as db:
        expected = r.app.size(r.app.refresh(db), body)
    # The broker accepted the order but the response was lost and the order is not yet listed.
    state = r.read(); state['fail'] = True; r.write(state)
    assert r.execute()['decisions'][0]['state'] == 'unknown'
    with pytest.raises(ValueError, match='not awaiting execution'):
        r.request()
    assert r.step()['decisions'][0]['state'] == 'unknown'
    assert len(r.submits()) == 1
    state = r.read(); del state['fail']
    state['snapshot']['orders'].append(expected | {'order_id': 'lost-order', 'executed_quantity': '0',
                                                   'quantity': str(expected['quantity']), 'status': 'New'})
    r.write(state)
    r.restart()
    assert r.step()['decisions'][0]['broker_id'] == 'lost-order'
    r.step()
    assert len(r.submits()) == 1
    r.fill('lost-order', 60, '4000', 'recovered-fill', 60)
    assert r.step()['decisions'][0]['state'] == 'settled'
    assert len(r.submits()) == 1


def test_spy_unknown_without_broker_match_stays_blocked(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['fail'] = True; r.write(state)
    assert r.execute()['decisions'][0]['state'] == 'unknown'
    assert r.step()['decisions'][0]['state'] == 'unknown'
    assert len(r.submits()) == 1


def test_spy_partial_fill_and_conflicting_execution_rollback(allocation_rig):
    r = allocation_rig; r.plan(); r.execute(); r.publish()
    r.fill('order-1', 20, '8000', 'partial-1', 20, 'PartialFilled')
    assert r.status()['decisions'][0]['state'] == 'working'
    r.step(); assert len(r.submits()) == 1
    state = r.read(); state['snapshot']['fills'][0]['price'] = '101'; r.write(state)
    before = r.status()
    after = r.step()
    assert 'conflicting' in after['error']
    for key in ('snapshot', 'decisions', 'fills'):
        assert after[key] == before[key]


def test_spy_external_position_blocks_execution(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['snapshot']['shares'] = state['snapshot']['available_shares'] = 1; r.write(state)
    state = r.execute()
    assert 'holdings disagree' in state['error'] and state['decisions'][0]['state'] == 'requested'
    assert not r.submits()


def test_spy_waits_for_regular_session_and_expires(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['snapshot']['market_open'] = False; r.write(state)
    decision = r.execute()['decisions'][0]
    assert decision['state'] == 'requested' and 'regular session' in decision['error']
    r.app.clock = lambda: NOW + timedelta(hours=2)
    assert r.step()['decisions'][0]['state'] == 'expired'
    assert not r.submits()


def test_spy_expired_decision_is_refused_at_request_time(allocation_rig):
    r = allocation_rig; r.plan()
    r.app.clock = lambda: NOW + timedelta(hours=2)
    with pytest.raises(ValueError, match='expired'):
        r.request()
    assert r.status()['decisions'][0]['state'] == 'queued'
    assert r.step()['decisions'][0]['state'] == 'expired'
    with pytest.raises(ValueError, match='not awaiting execution'):
        r.request()
    assert not r.submits()


def test_spy_stale_quote_waits_then_executes_fresh(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['snapshot']['quote']['at'] = (NOW - timedelta(minutes=3)).isoformat(); r.write(state)
    decision = r.execute()['decisions'][0]
    assert decision['state'] == 'requested' and 'stale' in decision['error']
    assert not r.submits()
    state = r.read(); state['snapshot']['quote']['at'] = NOW.isoformat(); r.write(state)
    decision = r.step()['decisions'][0]
    assert decision['state'] == 'working' and decision['error'] is None
    assert len(r.submits()) == 1


def test_spy_zero_target_noop_and_cash_buffer(allocation_rig):
    r = allocation_rig; r.plan(target_spy_bps=0)
    assert r.execute()['decisions'][0]['state'] == 'noop'
    r.plan(decision_id='full', target_spy_bps=10000); r.execute('full')
    assert r.order()['quantity'] == 97  # available cash minus buffer, then market-price reserve
    assert r.order()['quantity'] * 100 < 9800


def test_spy_insufficient_cash_and_maximum_order(allocation_rig):
    r = allocation_rig; r.plan()
    state = r.read(); state['snapshot']['available_cash_usd'] = '50'; r.write(state)
    assert 'settled cash' in r.execute()['decisions'][0]['error']
    assert not r.submits()
    state['snapshot']['available_cash_usd'] = '10000'; r.write(state)
    r.config = AllocationConfig.parse(r.values | {'max_order_bps': 500})
    r.restart()
    r.step()
    assert r.order()['quantity'] == 4
    assert r.order()['quantity'] * 100 * 1.01 <= 500


def test_spy_transport_environment_allowlist(allocation_rig, monkeypatch):
    monkeypatch.setenv('LONGBRIDGE_HTTP_URL', 'https://untrusted.example')
    monkeypatch.setenv('MODEL_API_KEY', 'fixture-secret')
    r = allocation_rig; r.step()
    assert 'LONGBRIDGE_HTTP_URL' not in r.calls()[0]['env_keys']
    assert 'MODEL_API_KEY' not in r.calls()[0]['env_keys']


def wait_for(host, predicate, caller=WORKER):
    deadline = time.monotonic() + 20
    while True:
        state = host.tool('spy.status', {}, track='owner', caller=caller)['structuredContent']
        if predicate(state):
            return state
        assert time.monotonic() < deadline, state
        time.sleep(0.05)


def test_spy_production_stdio_entrypoint_and_overlays(allocation_rig):
    r = allocation_rig
    now = datetime.now(timezone.utc)
    state = r.read(); state['snapshot']['quote']['at'] = now.isoformat(); r.write(state)
    host = Host(r.home, r.root, r.values)
    try:
        names = {t['name'] for t in host.request('tools/list', {})['tools']}
        assert names == {'spy.plan', 'spy.execute', 'spy.status', 'spy.refresh'}
        args = {'decision_id': 'stdio-target', 'target_spy_bps': 6000,
                'rationale': 'Research and live market data support the target allocation.',
                'source_refs': ['neige://source/frozen-research'],
                'valid_until': (now + timedelta(hours=1)).isoformat()}
        assert host.tool('spy.plan', args, track='owner', caller=WORKER)['isError']
        assert host.tool('spy.status', {}, track='owner')['isError']
        result = host.tool('spy.plan', args, track='owner', caller=PLANNER)['structuredContent']
        assert result['decisions'][0]['state'] == 'queued'
        assert host.tool('spy.execute', {'decision_id': 'stdio-target'}, track='owner', caller=PLANNER)['isError']
        result = host.tool('spy.execute', {'decision_id': 'stdio-target'}, track='owner', caller=WORKER)['structuredContent']
        assert result['decisions'][0]['state'] == 'requested'
        # The request woke the background loop, which owns the broker write.
        wait_for(host, lambda s: s['decisions'][0]['state'] == 'working')
        assert r.order()['quantity'] == 60
        # Prescribe actual broker records, then reconcile through the production stdio tool.
        r.publish(); state = r.read(); s = state['snapshot']; s['orders'][0]['status'] = 'Filled'; s['orders'][0]['executed_quantity'] = '60'
        s['shares'] = s['available_shares'] = 60; s['cash_usd'] = s['available_cash_usd'] = '4000'
        s['fills'] = [{'trade_id': 'stdio-fill', 'order_id': 'order-1', 'symbol': 'SPY.US', 'quantity': '60', 'price': '100', 'time': now.isoformat()}]
        r.write(state)
        assert not host.tool('spy.refresh', {}, track='owner', caller=WORKER).get('isError')
        result = wait_for(host, lambda s: s['decisions'][0]['state'] == 'settled')
        assert result['snapshot']['shares'] == 60
        # The runtime republishes every unit each poll tick; a missing kind fails at the deadline, never hangs.
        expected, deadline = unit_kinds(), time.monotonic() + 20
        while not expected <= {p['kind'] for p in host.overlays}:
            assert time.monotonic() < deadline, expected - {p['kind'] for p in host.overlays}
            host.receive()
        for overlay in host.overlays:
            # The kernel callback frame: every overlay belongs to the owning Track and carries its projection.
            assert set(overlay) == {'entity_kind', 'entity_id', 'kind', 'payload'}, overlay
            assert overlay['entity_kind'] == 'track' and overlay['entity_id'] == 'owner'
            assert isinstance(overlay['payload'], dict) and overlay['payload'], overlay['kind']
        # A replayed Worker request may read, but may never write another order.
        assert host.tool('spy.execute', {'decision_id': 'stdio-target'}, track='owner', caller=WORKER)['isError']
        assert len(r.submits()) == 1
    finally:
        host.close()


def test_spy_intent_is_durable_before_broker_write(allocation_rig):
    r = allocation_rig; r.plan(); r.execute()
    call = r.submits()[0]
    assert call['persisted_state'] == 'submitting'
    assert call['persisted_request'] == call['request']


def test_spy_terminal_retry_preserves_decision(allocation_rig):
    r = allocation_rig; r.plan(); r.execute(); r.publish(); r.fill('order-1', 60, '4000', 'filled', 60)
    before = r.status()['decisions']
    with pytest.raises(ValueError, match='settled, not awaiting execution'):
        r.request()
    r.step()
    assert r.status()['decisions'] == before
    assert len(r.submits()) == 1


def test_spy_default_step_is_ten_percent_of_portfolio(allocation_rig):
    r = allocation_rig
    values = {k: v for k, v in r.values.items() if k != 'max_order_bps'}
    r.config = AllocationConfig.parse(values)
    r.restart()
    r.plan(); r.execute()
    assert r.config.max_order_bps == 1000
    assert r.order()['quantity'] == 9
    assert r.order()['quantity'] * 100 * 1.01 <= 1000


def test_spy_proved_not_submitted_resolves_without_unknown(allocation_rig):
    r = allocation_rig; r.plan()
    state = r.read(); state['response'] = {'status': 'not_submitted'}; r.write(state)
    assert r.execute()['decisions'][0]['state'] == 'rejected'
    r.step()
    assert len(r.submits()) == 1
    r.plan(decision_id='fresh-target')
    assert r.status()['decisions'][-1]['state'] == 'queued'


def test_spy_decision_expiring_between_reconcile_and_submit_is_never_submitted(allocation_rig):
    r = allocation_rig
    deadline = NOW + timedelta(hours=1)
    r.plan(valid_until=deadline.isoformat())
    state = r.read(); state['snapshot']['quote']['at'] = (deadline - timedelta(seconds=1)).isoformat(); r.write(state)
    r.request()
    times = [deadline - timedelta(seconds=1)]
    r.app.clock = lambda: times[0]

    class LateBroker(AllocationBroker):
        def snapshot(self, since):
            raw = super().snapshot(since)
            # Reconciliation observes the decision as valid; the deadline passes before submission.
            r.app.clock = iter([times[0]] + [deadline + timedelta(seconds=1)] * 10).__next__
            return raw

    r.app.broker = LateBroker(r.config, str(r.root))
    assert r.step()['decisions'][0]['state'] == 'expired'
    assert not r.submits()


def test_spy_settled_history_leaves_the_reconciliation_window(allocation_rig):
    r = allocation_rig; r.plan(); r.execute(); r.publish(); r.fill('order-1', 60, '4000', 'buy-fill', 60)
    assert r.status()['decisions'][0]['state'] == 'settled'
    # Next trading day: the settled order and its execution are no longer in today's broker lists.
    state = r.read(); state['snapshot']['orders'] = []; state['snapshot']['fills'] = []; r.write(state)
    after = r.step()
    assert after['error'] is None and after['snapshot']['shares'] == 60
    assert after['decisions'][0]['state'] == 'settled' and len(after['fills']) == 1
    assert [c['request'] for c in r.calls() if c['method'] == 'snapshot'][-1] == {'since': None}


@pytest.mark.parametrize('key', ['Allocation-1', 'allocation_1', 'allocation.1', 'a' * 56])
def test_spy_decision_id_fits_the_worker_task_key(allocation_rig, key):
    with pytest.raises(ValueError, match='decision_id'):
        allocation_rig.plan(decision_id=key)
    assert allocation_rig.status()['decisions'] == []


def test_spy_longest_decision_id_still_plans(allocation_rig):
    assert allocation_rig.plan(decision_id='a' * 55)['decisions'][0]['id'] == 'a' * 55


def test_spy_unsubmitted_decision_expires_while_the_broker_is_unavailable(allocation_rig):
    r = allocation_rig; r.plan(); r.request()
    state = r.read(); state['snapshot']['identity']['account_channel'] = 'lb'; r.write(state)
    r.app.clock = lambda: NOW + timedelta(hours=2)
    after = r.step()
    assert 'identity' in after['error']
    assert after['decisions'][0]['state'] == 'expired' and not r.submits()
    r.plan(decision_id='replacement', valid_until=(NOW + timedelta(hours=3)).isoformat())
    assert r.status()['decisions'][-1]['state'] == 'queued'


@pytest.mark.parametrize('trading_day,half_day', [(True, False), (True, True), (False, False)])
def test_spy_status_snapshot_carries_the_broker_trading_day(allocation_rig, trading_day, half_day):
    r = allocation_rig
    state = r.read(); state['snapshot']['quote'] |= {'trading_day': trading_day, 'half_day': half_day}; r.write(state)
    r.step()
    snapshot = r.status()['snapshot']
    assert {k: snapshot[k] for k in ('calendar_date', 'trading_day', 'half_day', 'regular_close_at')} == {
        'calendar_date': '2026-09-30', 'trading_day': trading_day, 'half_day': half_day,
        'regular_close_at': '2026-09-30T20:00:00+00:00'}


@pytest.mark.parametrize('change', [
    {'trading_day': None}, {'half_day': None}, {'trading_day': None, 'half_day': None},
    {'trading_day': False, 'half_day': True},
    {'calendar_date': None}, {'calendar_date': 20260930}, {'calendar_date': '20260930'}, {'calendar_date': '2026-09-31'},
    {'regular_close_at': None}, {'regular_close_at': '2026-09-30T16:00:00'}, {'regular_close_at': 'tomorrow'}])
def test_spy_snapshot_without_a_broker_calendar_is_refused_not_guessed(allocation_rig, change):
    r = allocation_rig
    first = r.step()
    assert first['error'] is None and first['snapshot']['trading_day'] is True
    state = r.read(); quote = state['snapshot']['quote']
    for key, value in change.items():
        if value is None:
            quote.pop(key)
        else:
            quote[key] = value
    r.write(state)
    r.app.clock = lambda: NOW + timedelta(minutes=1)
    after = r.step()
    assert 'trading calendar' in after['error'] and after['snapshot'] == first['snapshot']


def test_spy_agent_text_never_sends_the_planner_to_the_longbridge_cli():
    manifest = json.loads((ROOT / 'manifest.json').read_text())
    texts = {'spy-recipe.md': (ROOT / 'spy-recipe.md').read_text()}
    texts |= {tool['name']: json.dumps(tool, ensure_ascii=False) for tool in manifest['exposes_tools']}
    assert len(texts) == len(TOOLS) + 1
    for name, text in texts.items():
        assert not re.search(r'longbridge|\bcli\b|k-line', text, flags=re.I), name


def _opening(r, opening, shares):
    """Broker holding `shares` before this ledger's first reconciliation, acknowledged as `opening`."""
    values = r.values if opening is None else r.values | {'opening_shares': opening}
    r.config = AllocationConfig.parse(values)
    r.restart()
    state = r.read(); state['snapshot']['shares'] = state['snapshot']['available_shares'] = shares; r.write(state)


def test_spy_acknowledged_opening_shares_reconcile_and_can_be_traded(allocation_rig):
    r = allocation_rig; _opening(r, 13, 13)
    state = r.step()
    assert state['error'] is None and state['snapshot']['shares'] == 13
    assert [v['shares'] for v in state['valuations']] == [13]
    # A later Buy of 54 reconciles against 13 + 54 owned shares.
    r.plan(); r.execute()
    assert r.order()['side'] == 'Buy' and r.order()['quantity'] == 54
    r.publish(); after = r.fill('order-1', 67, '4600', 'buy-fill', 54)
    assert after['error'] is None and after['snapshot']['shares'] == 67
    # Acknowledged opening shares belong to the portfolio: a zero target sells them too.
    r.plan(decision_id='allocation-2', target_spy_bps=0)
    state = r.read(); state['response'] = {'order_id': 'order-2'}; r.write(state)
    r.execute('allocation-2')
    assert r.order()['side'] == 'Sell' and r.order()['quantity'] == 67
    r.publish('order-2'); final = r.fill('order-2', 0, '11300', 'sell-fill', 67)
    assert final['error'] is None and final['snapshot']['shares'] == 0
    assert [d['state'] for d in final['decisions']] == ['settled', 'settled']


@pytest.mark.parametrize('opening,shares', [(None, 13), (13, 12), (13, 14), (0, 13)],
                         ids=['omitted', 'broker-below', 'broker-above', 'explicit-zero'])
def test_spy_opening_shares_mismatch_blocks_first_reconciliation(allocation_rig, opening, shares):
    r = allocation_rig; _opening(r, opening, shares); r.plan()
    state = r.execute()
    assert 'holdings disagree' in state['error'] and state['snapshot'] is None and state['valuations'] == []
    assert state['decisions'][0]['state'] == 'requested' and not r.submits()
    # Nothing was pinned: correcting the acknowledgement to the real holding reconciles.
    _opening(r, shares, shares)
    assert r.step()['snapshot']['shares'] == shares


@pytest.mark.parametrize('changed', [None, 12, 14])
def test_spy_opening_shares_cannot_change_on_an_existing_ledger(allocation_rig, changed):
    r = allocation_rig; _opening(r, 13, 13)
    before = r.step()
    _opening(r, changed, 13 if changed is None else changed)
    after = r.step()
    assert 'opening_shares cannot change' in after['error']
    assert after['snapshot'] == before['snapshot'] and after['valuations'] == before['valuations']


def test_spy_ledger_reconciled_before_opening_shares_started_from_zero(allocation_rig):
    r = allocation_rig
    assert r.step()['snapshot']['shares'] == 0
    with r.app.ledger.session() as db:
        db.execute("DELETE FROM meta WHERE key='opening_shares'")  # a ledger from before the field
    _opening(r, 13, 13)
    assert 'opening_shares cannot change' in r.step()['error']


@pytest.mark.parametrize('value', [-1, 1.0, '13', True, None])
def test_spy_opening_shares_config_is_a_non_negative_integer(allocation_rig, value):
    with pytest.raises(ValueError, match='opening_shares'):
        AllocationConfig.parse(allocation_rig.values | {'opening_shares': value})


def test_spy_config_requires_a_profile(allocation_rig):
    values = {k: v for k, v in allocation_rig.values.items() if k != 'profile'}
    with pytest.raises(ValueError, match='missing or unknown fields'):
        AllocationConfig.parse(values)


@pytest.mark.parametrize('profile', ['spy-cash', 'SPY_CASH', '', None])
def test_spy_config_refuses_any_profile_but_spy_cash(allocation_rig, profile):
    with pytest.raises(ValueError, match='spy_cash profile'):
        AllocationConfig.parse(allocation_rig.values | {'profile': profile})


def test_spy_config_refuses_an_unknown_key(allocation_rig):
    with pytest.raises(ValueError, match='missing or unknown fields'):
        AllocationConfig.parse(allocation_rig.values | {'unexpected_setting': 1})


def test_manifest_exposes_exactly_the_app_tools():
    names = [tool['name'] for tool in json.loads((ROOT / 'manifest.json').read_text())['exposes_tools']]
    assert len(names) == len(TOOLS) and set(names) == TOOLS


def test_manifest_config_schema_matches_the_parsed_keys():
    schema = json.loads((ROOT / 'manifest.json').read_text())['config_schema']
    assert set(schema['properties']) == REQUIRED | OPTIONAL
    assert set(schema['required']) == REQUIRED
    assert len(schema['required']) == len(REQUIRED)


@pytest.mark.parametrize('field,value', [('owner_track_id', 'other-owner'), ('account_no', 'PAPER456')])
def test_spy_ledger_account_and_track_binding_cannot_change_on_restart(allocation_rig, field, value):
    r = allocation_rig; r.plan()
    r.config = AllocationConfig.parse(r.values | {field: value})
    with pytest.raises(ValueError, match='binding cannot be changed'):
        r.restart()
    # The original binding still opens the ledger, with its decision intact.
    r.config = AllocationConfig.parse(r.values)
    r.restart()
    assert [d['id'] for d in r.status()['decisions']] == ['allocation-1']


@pytest.mark.parametrize('field,value', [('oauth_client_id', 'other-client'), ('broker_home', '/other/home')])
def test_spy_execution_binding_cannot_change_on_restart(allocation_rig, field, value):
    r = allocation_rig; r.plan()
    r.config = AllocationConfig.parse(r.values | {field: value})
    with pytest.raises(ValueError, match='SPY execution binding cannot change'):
        r.restart()
    r.config = AllocationConfig.parse(r.values)
    r.restart()
    assert [d['id'] for d in r.status()['decisions']] == ['allocation-1']


class ProcessCrash(BaseException):
    """Stands in for the process dying inside the broker write; no handler may absorb it."""


def test_spy_restart_marks_a_crashed_submission_unknown_and_never_resubmits(allocation_rig, monkeypatch):
    r = allocation_rig; r.plan(); r.request()

    def crash(request):
        raise ProcessCrash()
    monkeypatch.setattr(r.broker, 'submit', crash)
    with pytest.raises(ProcessCrash):
        r.step()
    with r.app.ledger.session() as db:
        assert r.app.ledger.decision(db, 'allocation-1')['state'] == 'submitting'
    monkeypatch.undo()
    r.restart()
    state = r.status()
    assert state['decisions'][0]['state'] == 'unknown'
    assert any(e['kind'] == 'submission_unknown' for e in state['journal'])
    # Later passes reconcile only: the uncertain order is never written again.
    for _ in range(2):
        assert r.step()['decisions'][0]['state'] == 'unknown'
    assert r.submits() == []


def test_spy_unchanged_reconciliation_appends_no_false_transition(allocation_rig):
    r = allocation_rig; r.plan(); r.execute(); r.publish()
    first = r.step()
    assert first['error'] is None and first['decisions'][0]['state'] == 'working'
    # Every poll re-applies each owned order's state; unchanged broker records record nothing.
    second = r.step()
    assert second['decisions'][0]['state'] == 'working'
    assert second['journal'] == first['journal']


def test_spy_table_units_publish_only_declared_columns(allocation_rig):
    r = allocation_rig; r.plan(); r.execute(); r.publish()
    state = r.fill('order-1', 60, '4000', 'buy-fill', 60)
    assert state['error'] is None and state['fills']
    published = {kind: unit['cell']['table'] for kind, unit in units(state).items() if unit['cell']['kind'] == 'table'}
    assert set(published) == {'spy.holdings', 'spy.fill_log'}
    for kind, payload in published.items():
        assert set(payload) == {'columns', 'rows', 'caption'}, kind
        assert isinstance(payload['caption'], str) and payload['caption'], kind
        assert all({'key', 'label'} <= set(column) <= {'key', 'label', 'align'} and column['label']
                   for column in payload['columns']), kind
        declared = {column['key'] for column in payload['columns']}
        assert payload['rows'], kind
        # The kernel refuses a row key that is not a declared column.
        assert all(set(row) == declared for row in payload['rows']), kind


def test_spy_status_names_its_profile_and_symbol(allocation_rig):
    state = allocation_rig.status()
    assert state['profile'] == 'spy_cash' and state['symbol'] == 'SPY.US'


def test_spy_ledger_persists_its_account_and_track_binding(allocation_rig):
    with allocation_rig.app.ledger.session() as db:
        stored = db.execute("SELECT body FROM meta WHERE key='binding'").fetchone()[0]
    # The live ledger's persisted form: renaming either key would orphan an existing binding.
    assert stored == '{"account_no":"PAPER123","owner_track_id":"owner"}'


def test_spy_settlement_journals_one_decision_state_transition(allocation_rig):
    r = allocation_rig; r.plan(); r.execute(); r.publish()
    before = {e['seq'] for e in r.status()['journal']}
    # One reconciliation moves the working order straight to settled.
    state = r.fill('order-1', 60, '4000', 'buy-fill', 60)
    added = [e for e in state['journal'] if e['seq'] not in before and e['kind'] == 'decision_state']
    assert [e['body'] for e in added] == [{'decision_id': 'allocation-1', 'state': 'settled', 'error': None,
                                          'broker_id': 'order-1', 'broker_status': 'Filled'}]


@pytest.mark.parametrize('status,expected,executed', [
    ('Canceled', 'canceled', 0), ('Rejected', 'rejected', 0), ('Expired', 'expired', 0),
    ('PartialWithdrawal', 'canceled', 30)])
def test_spy_terminal_broker_status_resolves_the_decision(allocation_rig, status, expected, executed):
    r = allocation_rig; r.plan(); r.execute()
    if executed:
        r.publish(); state = r.fill('order-1', executed, '7000', 'part-fill', executed, status=status)
    else:
        r.publish(status=status); state = r.step()
    assert state['error'] is None
    assert state['decisions'][0]['state'] == expected
    assert state['decisions'][0]['broker_status'] == status


def test_spy_exponent_broker_price_is_refused_before_any_order(allocation_rig):
    r = allocation_rig; r.plan()
    state = r.read(); state['snapshot']['quote']['price'] = '1E+2'; r.write(state)
    state = r.execute()
    # Sizing reads only plain decimal strings; the decision waits instead of trading on it.
    assert 'without exponent' in state['decisions'][0]['error']
    assert state['decisions'][0]['state'] == 'requested' and not r.submits()


def test_spy_fully_invested_account_with_zero_cash_can_sell(allocation_rig):
    r = allocation_rig; _opening(r, 100, 100)
    state = r.read(); state['snapshot']['cash_usd'] = state['snapshot']['available_cash_usd'] = '0'; r.write(state)
    r.plan(target_spy_bps=0)
    state = r.execute()
    assert state['error'] is None and state['snapshot']['cash_usd'] == '0'
    assert state['decisions'][0]['state'] == 'working'
    assert r.order()['side'] == 'Sell' and r.order()['quantity'] > 0


def test_spy_zero_broker_price_fails_reconciliation(allocation_rig):
    r = allocation_rig; r.plan()
    state = r.read(); state['snapshot']['quote']['price'] = '0'; r.write(state)
    state = r.execute()
    # Cash may be zero; a quote price may not. The observation is refused as a whole.
    assert 'outside supported range' in state['error'] and state['snapshot'] is None
    assert state['decisions'][0]['state'] == 'requested' and not r.submits()
