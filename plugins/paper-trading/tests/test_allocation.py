"""Production allocation and subprocess entry points, with prescribed external records."""
from datetime import datetime, timedelta, timezone
from decimal import Decimal
import json
from pathlib import Path

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
              'max_order_usd': '100000', 'poll_seconds': 5, 'drift_bps': 0}
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

        def execute(self, key='allocation-1'):
            return self.app.call('owner', 'spy.execute', {'decision_id': key}, WORKER)

        def calls(self):
            p = home / 'allocation-calls.jsonl'
            return [json.loads(l) for l in p.read_text().splitlines()] if p.exists() else []

        def request(self):
            return [c['request'] for c in self.calls() if c['method'] == 'submit'][-1]

        def publish(self, key='order-1', executed=0, status='New'):
            state = self.read(); request = self.request()
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
            self.app.refresh()

        def status(self):
            return self.app.call('owner', 'spy.status', {}, PLANNER)

    return Rig()


def test_spy_buy_sell_and_restart_closed_loop(allocation_rig):
    r = allocation_rig
    r.plan(); result = r.execute()
    assert result['decisions'][0]['state'] == 'working'
    assert r.request()['side'] == 'Buy' and r.request()['quantity'] == 60
    assert r.request()['symbol'] == 'SPY.US' and r.request()['order_type'] == 'MO'
    r.publish(); r.fill('order-1', 60, '4000', 'buy-fill', 60)
    assert r.status()['snapshot']['actual_spy_bps'] == '6000.0'
    assert r.status()['decisions'][0]['state'] == 'settled'
    r.plan(decision_id='allocation-2', target_spy_bps=3000)
    state = r.read(); state['response'] = {'order_id': 'order-2'}; r.write(state)
    r.execute('allocation-2')
    assert r.request()['side'] == 'Sell' and r.request()['quantity'] == 30
    r.publish('order-2'); r.fill('order-2', 30, '7000', 'sell-fill', 30)
    r.app = Allocation(r.root, r.config, r.broker, clock=lambda: NOW)
    r.app.refresh(); state = r.status()
    assert state['snapshot']['shares'] == 30 and Decimal(state['snapshot']['actual_spy_bps']) == 3000
    assert [d['state'] for d in state['decisions']] == ['settled', 'settled']
    assert len(state['fills']) == 2


@pytest.mark.parametrize('name,role,args', [('spy.plan', WORKER, {}), ('spy.execute', PLANNER, {'decision_id': 'allocation-1'})])
def test_spy_role_fence(allocation_rig, name, role, args):
    r = allocation_rig
    with pytest.raises(ValueError, match='identity'):
        r.app.call('owner', name, args, role)
    assert not r.calls()


@pytest.mark.parametrize('caller', [None, {}, {'role': 'worker'}, PLANNER | {'card_id': ''}])
def test_spy_missing_caller_fence(allocation_rig, caller):
    with pytest.raises(ValueError, match='identity'):
        allocation_rig.app.call('owner', 'spy.status', {}, caller)


def test_spy_track_fence(allocation_rig):
    with pytest.raises(ValueError, match='own'):
        allocation_rig.app.call('other', 'spy.execute', {'decision_id': 'allocation-1'}, WORKER)


@pytest.mark.parametrize('change', [{'account_channel': 'lb'}, {'account_no': 'OTHER'}, {'account_channel': None}])
def test_spy_paper_account_fence(allocation_rig, change):
    r = allocation_rig; r.plan()
    state = r.read(); state['snapshot']['identity'].update(change); r.write(state)
    with pytest.raises(ValueError, match='identity'):
        r.execute()
    assert not [c for c in r.calls() if c['method'] == 'submit']


def test_spy_target_immutable_and_one_pending_decision(allocation_rig):
    r = allocation_rig; r.plan(); r.plan()
    with pytest.raises(ValueError, match='different target'):
        r.plan(target_spy_bps=5000)
    with pytest.raises(ValueError, match='resolve'):
        r.plan(decision_id='second')
    assert len(r.status()['decisions']) == 1


def test_spy_no_repeat_submit_after_lost_response(allocation_rig):
    r = allocation_rig; r.plan()
    expected = r.app.size(r.app.refresh(), r.status()['decisions'][0]['body'])
    state = r.read(); state.update({'fail': True, 'publish': expected | {'order_id': 'lost-order',
                                  'executed_quantity': '0', 'quantity': str(expected['quantity']), 'status': 'New'}})
    r.write(state); assert r.execute()['decisions'][0]['state'] == 'unknown'
    r.app = Allocation(r.root, r.config, r.broker, clock=lambda: NOW)
    assert r.execute()['decisions'][0]['broker_id'] == 'lost-order'
    r.execute()
    assert len([c for c in r.calls() if c['method'] == 'submit']) == 1
    r.fill('lost-order', 60, '4000', 'recovered-fill', 60)
    assert r.execute()['decisions'][0]['state'] == 'settled'
    assert len([c for c in r.calls() if c['method'] == 'submit']) == 1


def test_spy_unknown_without_broker_match_stays_blocked(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['fail'] = True; r.write(state)
    assert r.execute()['decisions'][0]['state'] == 'unknown'
    assert r.execute()['decisions'][0]['state'] == 'unknown'
    assert len([c for c in r.calls() if c['method'] == 'submit']) == 1


def test_spy_partial_fill_and_conflicting_execution_rollback(allocation_rig):
    r = allocation_rig; r.plan(); r.execute(); r.publish()
    r.fill('order-1', 20, '8000', 'partial-1', 20, 'PartialFilled')
    assert r.status()['decisions'][0]['state'] == 'working'
    r.execute(); assert len([c for c in r.calls() if c['method'] == 'submit']) == 1
    state = r.read(); state['snapshot']['fills'][0]['price'] = '101'; r.write(state)
    before = r.status()
    with pytest.raises(ValueError, match='conflicting'):
        r.app.refresh()
    assert r.status() == before


def test_spy_external_position_blocks_execution(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['snapshot']['shares'] = state['snapshot']['available_shares'] = 1; r.write(state)
    with pytest.raises(ValueError, match='holdings disagree'):
        r.execute()
    assert not [c for c in r.calls() if c['method'] == 'submit']


def test_spy_waits_for_regular_session_and_expires(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['snapshot']['market_open'] = False; r.write(state)
    assert r.execute()['decisions'][0]['state'] == 'queued'
    r.app.clock = lambda: NOW + timedelta(hours=2)
    r.app.refresh(); assert r.status()['decisions'][0]['state'] == 'expired'
    assert not [c for c in r.calls() if c['method'] == 'submit']


def test_spy_stale_quote_blocks_execution(allocation_rig):
    r = allocation_rig; r.plan(); state = r.read(); state['snapshot']['quote']['at'] = (NOW - timedelta(minutes=3)).isoformat(); r.write(state)
    with pytest.raises(ValueError, match='stale'):
        r.execute()
    assert not [c for c in r.calls() if c['method'] == 'submit']


def test_spy_zero_target_noop_and_cash_buffer(allocation_rig):
    r = allocation_rig; r.plan(target_spy_bps=0)
    assert r.execute()['decisions'][0]['state'] == 'noop'
    r.plan(decision_id='full', target_spy_bps=10000); r.execute('full')
    assert r.request()['quantity'] == 97  # available cash minus buffer, then market-price reserve
    assert r.request()['quantity'] * 100 < 9800


def test_spy_insufficient_cash_and_maximum_order(allocation_rig):
    r = allocation_rig; r.plan()
    state = r.read(); state['snapshot']['available_cash_usd'] = '50'; r.write(state)
    with pytest.raises(ValueError, match='settled cash'):
        r.execute()
    state['snapshot']['available_cash_usd'] = '10000'; r.write(state)
    r.config = AllocationConfig.parse(r.values | {'max_order_usd': '500'})
    r.app = Allocation(r.root, r.config, r.broker, clock=lambda: NOW)
    with pytest.raises(ValueError, match='maximum order'):
        r.execute()


def test_spy_transport_environment_allowlist(allocation_rig, monkeypatch):
    monkeypatch.setenv('LONGBRIDGE_HTTP_URL', 'https://untrusted.example')
    monkeypatch.setenv('MODEL_API_KEY', 'fixture-secret')
    r = allocation_rig; r.app.refresh()
    assert 'LONGBRIDGE_HTTP_URL' not in r.calls()[0]['env_keys']
    assert 'MODEL_API_KEY' not in r.calls()[0]['env_keys']


def test_spy_production_stdio_entrypoint_and_overlays(allocation_rig):
    from types import SimpleNamespace
    from .test_process import Host
    r=allocation_rig
    now=datetime.now(timezone.utc)
    state=r.read();state['snapshot']['quote']['at']=now.isoformat();r.write(state)
    host=Host(SimpleNamespace(home=r.home,data=r.root),config=r.values | {'cli_path':'/usr/local/bin/longbridge'})
    try:
        names={t['name'] for t in host.request('tools/list',{})['tools']}
        assert names=={'spy.plan','spy.execute','spy.status','spy.refresh'}
        args={'decision_id':'stdio-target','target_spy_bps':6000,
              'rationale':'Research and live market data support the target allocation.',
              'source_refs':['neige://source/frozen-research'],
              'valid_until':(now+timedelta(hours=1)).isoformat()}
        assert host.tool('spy.plan',args,track='owner',caller=WORKER)['isError']
        assert host.tool('spy.status',{},track='owner')['isError']
        result=host.tool('spy.plan',args,track='owner',caller=PLANNER)['structuredContent']
        assert result['decisions'][0]['state']=='queued'
        assert host.tool('spy.execute',{'decision_id':'stdio-target'},track='owner',caller=PLANNER)['isError']
        result=host.tool('spy.execute',{'decision_id':'stdio-target'},track='owner',caller=WORKER)['structuredContent']
        assert result['decisions'][0]['state']=='working'
        assert r.request()['quantity']==60
        # Prescribe actual broker records, then reconcile through the production stdio tool.
        r.publish();state=r.read();s=state['snapshot'];s['orders'][0]['status']='Filled';s['orders'][0]['executed_quantity']='60'
        s['shares']=s['available_shares']=60;s['cash_usd']=s['available_cash_usd']='4000'
        s['fills']=[{'trade_id':'stdio-fill','order_id':'order-1','symbol':'SPY.US','quantity':'60','price':'100','time':now.isoformat()}]
        r.write(state)
        result=host.tool('spy.refresh',{},track='owner',caller=WORKER)['structuredContent']
        assert result['decisions'][0]['state']=='settled'
        assert result['snapshot']['shares']==60
        while len({p['kind'] for p in host.overlays})<3:
            host.receive()
        assert {p['kind'] for p in host.overlays}=={'spy.portfolio','spy.decisions','spy.fills'}
        # Replayed worker completion/requests may read, but may never write another order.
        replay=host.tool('spy.execute',{'decision_id':'stdio-target'},track='owner',caller=WORKER)['structuredContent']
        assert replay['decisions'][0]['state']=='settled'
        assert not any(e['kind']=='allocation_noop' for e in replay['journal'])
        assert len([c for c in r.calls() if c['method']=='submit'])==1
    finally:
        host.close()



def test_spy_intent_is_durable_before_broker_write(allocation_rig):
    r=allocation_rig; r.plan();r.execute()
    call=next(c for c in r.calls() if c['method']=='submit')
    assert call['persisted_state']=='submitting'
    assert call['persisted_request']==call['request']


def test_spy_terminal_retry_preserves_decision(allocation_rig):
    r=allocation_rig;r.plan();r.execute();r.publish();r.fill('order-1',60,'4000','filled',60)
    before=r.status()['decisions']
    assert r.execute()['decisions']==before
    assert len([c for c in r.calls() if c['method']=='submit'])==1
