"""Production allocation and subprocess entry points, with prescribed external records."""
from datetime import datetime, timedelta, timezone
from decimal import Decimal
import json
from pathlib import Path
import time

import pytest

from paper_trading.allocation import Allocation
from paper_trading.allocation_broker import AllocationBroker
from paper_trading.allocation_config import AllocationConfig

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
                'quote': {'price': '100', 'at': NOW.isoformat(), 'status': 'Normal'},
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
    from types import SimpleNamespace
    from .test_process import Host
    r = allocation_rig
    now = datetime.now(timezone.utc)
    state = r.read(); state['snapshot']['quote']['at'] = now.isoformat(); r.write(state)
    host = Host(SimpleNamespace(home=r.home, data=r.root), config=r.values | {'cli_path': '/usr/local/bin/longbridge'})
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
        while not {'spy.portfolio', 'spy.decisions', 'spy.fills'} <= {p['kind'] for p in host.overlays}:
            host.receive()
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
