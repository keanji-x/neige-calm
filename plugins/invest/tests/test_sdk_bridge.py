"""The multi-symbol official-SDK adapter, against prescribed SDK response objects (no network)."""
from datetime import datetime, timezone
from decimal import Decimal
import json
import sys
from types import SimpleNamespace as NS

import pytest

from invest import sdk_bridge as bridge

NOW = datetime(2026, 9, 30, 15, tzinfo=timezone.utc)


@pytest.fixture
def sdk(monkeypatch):
    calls, quoted = [], []
    state = NS(positions={'AAA.US': (50, 50), 'BBB.US': (10, 10)}, total='5000', cash='5000', settling='0',
               prices={'AAA.US': '100', 'BBB.US': '200', 'CCC.US': '10'}, quote_at=NOW, status='Normal',
               channel='lb_papertrading', account='PAPER123', orders=[], currency='USD')

    class Trade:
        def stock_positions(self):
            return NS(channels=[NS(account_channel=state.channel, positions=[
                NS(symbol=s, currency=state.currency, quantity=Decimal(q), available_quantity=Decimal(a))
                for s, (q, a) in state.positions.items()])])

        def fund_positions(self):
            return NS(channels=[])

        def account_balance(self, currency):
            return [NS(currency='USD', total_cash=Decimal(state.total), buy_power=Decimal('1000000'),
                       cash_infos=[NS(currency='USD', available_cash=Decimal(state.cash),
                                      settling_cash=Decimal(state.settling))])]

        def today_orders(self):
            return state.orders

        def history_orders(self, start_at, end_at):
            return []

        def today_executions(self):
            return []

        def submit_order(self, **kwargs):
            calls.append(kwargs)
            return NS(order_id='sdk-order')

    class Quote:
        def quote(self, symbols):
            quoted.append(list(symbols))
            return [NS(symbol=s, last_done=Decimal(state.prices[s]), timestamp=state.quote_at,
                       trade_status='TradeStatus.' + state.status) for s in symbols if s in state.prices]

        def trading_days(self, market, begin, end):
            return NS(trading_days=[begin], half_trading_days=[])

    class Asset:
        def statements(self, kind, start_date, limit):
            return NS(list=[NS(file_key='statement')])

        def statement_download_url(self, key):
            return NS(url='https://statement.example/document')

    class Response:
        url = 'https://statement.example/document'
        def __enter__(self): return self
        def __exit__(self, *args): pass
        def read(self, size): return json.dumps({'MemberInfo': {'AccountNo': state.account}}).encode()

    monkeypatch.setattr(bridge.urllib.request, 'urlopen', lambda url, timeout: Response())
    module = NS(StatementType=NS(Daily='daily'), Market=NS(US='US'), OrderType=NS(MO='MO'),
                OrderSide=NS(Buy='Buy', Sell='Sell'), OutsideRTH=NS(RTHOnly='RTH_ONLY'),
                TimeInForceType=NS(Day='Day'))
    monkeypatch.setitem(sys.modules, 'longbridge.openapi', module)

    class Clock(datetime):
        @classmethod
        def now(cls, zone=None): return NOW
    monkeypatch.setattr(bridge, 'datetime', Clock)
    request = {'symbol': 'CCC.US', 'side': 'Buy', 'quantity': 40, 'order_type': 'MO', 'time_in_force': 'Day',
               'outside_rth': 'RTH_ONLY', 'client_request_id': 'a' * 64, 'remark': 'nc-inv-' + 'a' * 32,
               'basis_shares': 0, 'not_after': NOW.replace(hour=16).isoformat()}
    return NS(asset=Asset(), trade=Trade(), quote=Quote(), state=state, calls=calls, quoted=quoted,
              request=request, policy={'cash_buffer_bps': 200, 'max_order_bps': 1000, 'quote_max_age_seconds': 60})


def submit(sdk, **changes):
    return bridge.submit(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', sdk.request | changes, sdk.policy)


def test_snapshot_quotes_requested_and_held_symbols_in_one_call(sdk):
    raw = bridge.snapshot(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', {'since': None, 'symbols': ['CCC.US']})
    assert sdk.quoted == [['AAA.US', 'BBB.US', 'CCC.US']]
    assert raw['positions'] == {'AAA.US': {'shares': 50, 'available_shares': 50},
                                'BBB.US': {'shares': 10, 'available_shares': 10}}
    assert set(raw['quotes']) == {'AAA.US', 'BBB.US', 'CCC.US'} and raw['market_open'] is True


def test_submit_is_a_cash_funded_market_order_for_any_us_symbol(sdk):
    # Equity counts every position: 5000 + 50 * 100 + 10 * 200 = 12000, so a 10% step is 1200.
    assert submit(sdk) == {'order_id': 'sdk-order'}
    assert sdk.calls == [{'symbol': 'CCC.US', 'order_type': 'MO', 'side': 'Buy', 'submitted_quantity': Decimal(40),
                          'time_in_force': 'Day', 'outside_rth': 'RTH_ONLY', 'remark': 'nc-inv-' + 'a' * 32,
                          'client_request_id': 'a' * 64}]


@pytest.mark.parametrize('change', [
    {'symbol': '700.HK'}, {'symbol': 'CCC'}, {'order_type': 'LO'}, {'outside_rth': 'ANY_TIME'},
    {'quantity': True}, {'side': 'Short'}, {'remark': 'nc-spy-' + 'a' * 32}, {'quantity': 200},
    {'basis_shares': 1}, {'not_after': NOW.isoformat()}])
def test_preflight_refuses_before_any_write(sdk, change):
    with pytest.raises(bridge.OrderNotSubmitted):
        submit(sdk, **change)
    assert sdk.calls == []


@pytest.mark.parametrize('field,value', [('channel', 'lb'), ('account', 'OTHER'), ('status', 'Halted')])
def test_identity_and_trading_status_fence(sdk, field, value):
    setattr(sdk.state, field, value)
    with pytest.raises(bridge.OrderNotSubmitted):
        submit(sdk)
    assert sdk.calls == []


def test_sell_is_bounded_by_the_symbols_available_shares(sdk):
    sell = {'symbol': 'BBB.US', 'side': 'Sell', 'basis_shares': 10, 'quantity': 5}
    assert submit(sdk, **sell) == {'order_id': 'sdk-order'}
    sdk.state.positions['BBB.US'] = (10, 4)
    with pytest.raises(bridge.OrderNotSubmitted):
        submit(sdk, **sell)
    assert len(sdk.calls) == 1


def test_unsettled_proceeds_never_fund_a_buy(sdk):
    sdk.state.settling = '4900'
    assert bridge.cash(sdk.trade) == ('5000', '100')
    with pytest.raises(bridge.OrderNotSubmitted):
        submit(sdk)
    assert sdk.calls == []


@pytest.mark.parametrize('positions,match', [
    ({'700.HK': (1, 1)}, 'US'), ({'AAA.US': (-1, 0)}, 'position')])
def test_unsupported_positions_are_refused(sdk, positions, match):
    sdk.state.positions = positions
    with pytest.raises(ValueError, match=match):
        bridge.positions(sdk.trade)


def test_non_usd_position_is_refused(sdk):
    sdk.state.currency = 'HKD'
    with pytest.raises(ValueError, match='USD'):
        bridge.positions(sdk.trade)


def test_run_emits_exact_not_submitted_proof(monkeypatch, capsys):
    def fail_before_write():
        raise bridge.OrderNotSubmitted('private diagnostic')
    monkeypatch.setattr(bridge, 'main', fail_before_write)
    bridge.run()
    result = capsys.readouterr()
    assert json.loads(result.out) == {'status': 'not_submitted'} and 'private' not in result.err


def test_context_hard_codes_server_paper_enforcement(sdk, monkeypatch):
    options = []

    class OAuth:
        def __init__(self, client): pass
        def build(self, callback): return 'oauth'

    class Config:
        @staticmethod
        def from_oauth(oauth, **kwargs):
            options.append(kwargs)
            return 'config'
    module = sys.modules['longbridge.openapi']
    module.OAuthBuilder, module.Config = OAuth, Config
    module.AssetContext = module.TradeContext = module.QuoteContext = lambda config: config
    assert bridge.contexts('sdk-client', access_region='cn') == ('config', 'config', 'config')
    assert options[0]['enable_papertrading'] is True and options[0]['http_url'] == 'https://openapi.longbridge.cn'
