"""Exercise the real official-SDK adapter with prescribed SDK response objects."""
from datetime import datetime, timezone
from decimal import Decimal
import json
import sys
from types import SimpleNamespace as NS

import pytest

from paper_trading import sdk_bridge as bridge

NOW = datetime(2026, 9, 30, 15, tzinfo=timezone.utc)


@pytest.fixture
def sdk(monkeypatch):
    calls = []
    position = NS(account_channel='lb_papertrading', positions=[])
    state = NS(shares=0, available=0, total='10000', cash='10000', price='100',
               quote_at=NOW, channel='lb_papertrading', account='PAPER123', orders=[], half=False)

    class Trade:
        def stock_positions(self):
            return NS(channels=[NS(account_channel=state.channel, positions=(
                [NS(symbol='SPY.US', currency='USD', quantity=Decimal(state.shares),
                    available_quantity=Decimal(state.available))] if state.shares else []))])

        def fund_positions(self):
            return NS(channels=[])

        def account_balance(self, currency):
            assert currency == 'USD'
            return [NS(currency='USD', total_cash=Decimal(state.total), buy_power=Decimal('1000000'),
                       cash_infos=[NS(currency='USD', available_cash=Decimal(state.cash), settling_cash=Decimal(0))])]

        def today_orders(self):
            return state.orders

        def submit_order(self, **kwargs):
            calls.append(kwargs)
            return NS(order_id='sdk-order')

    class Quote:
        def quote(self, symbols):
            assert symbols == ['SPY.US']
            return [NS(symbol='SPY.US', last_done=Decimal(state.price), timestamp=state.quote_at,
                       trade_status='TradeStatus.Normal')]

        def trading_days(self, market, begin, end):
            assert begin == end
            return NS(trading_days=[begin], half_trading_days=[begin] if state.half else [])

    class Asset:
        def statements(self, kind, start_date, limit):
            assert start_date == 1 and limit == 1
            return NS(list=[NS(file_key='statement')])

        def statement_download_url(self, key):
            assert key == 'statement'
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
    monkeypatch.setattr(bridge, 'datetime', NS(now=lambda zone: NOW))
    request = {'symbol':'SPY.US','side':'Buy','quantity':60,'order_type':'MO','time_in_force':'Day',
               'outside_rth':'RTH_ONLY','client_request_id':'a' * 64,'remark':'nc-spy-' + 'a' * 32,'basis_shares':0}
    return NS(asset=Asset(), trade=Trade(), quote=Quote(), state=state, calls=calls,
              module=module, request=request,
              policy={'cash_buffer_bps':200,'max_order_usd':'100000','quote_max_age_seconds':60})


def test_sdk_submit_is_cash_funded_spy_market_order(sdk):
    result = bridge.submit(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', sdk.request, sdk.policy)
    assert result == {'order_id':'sdk-order'}
    assert sdk.calls == [{'symbol':'SPY.US','order_type':'MO','side':'Buy','submitted_quantity':Decimal(60),
                         'time_in_force':'Day','outside_rth':'RTH_ONLY',
                         'remark':'nc-spy-'+'a'*32, 'client_request_id':'a'*64}]


@pytest.mark.parametrize('field,value,match', [('channel','lb','paper-account'), ('account','OTHER','configured paper')])
def test_sdk_identity_fence(sdk, field, value, match):
    setattr(sdk.state,field,value)
    with pytest.raises(ValueError,match=match):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls


@pytest.mark.parametrize('change', [{'symbol':'QQQ.US'}, {'order_type':'LO'}, {'outside_rth':'ANY_TIME'},
                                    {'quantity':True}, {'side':'Short'}])
def test_sdk_rejects_unsupported_order(sdk,change):
    with pytest.raises(ValueError):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request | change,sdk.policy)
    assert not sdk.calls


def test_sdk_cash_recheck_does_not_use_margin_buying_power(sdk):
    sdk.state.cash='50'
    with pytest.raises(ValueError,match='settled cash'):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls


def test_sdk_holdings_recheck(sdk):
    sdk.state.shares=sdk.state.available=1
    with pytest.raises(ValueError,match='holdings changed'):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls


def test_sdk_freshness_recheck(sdk):
    sdk.state.quote_at=datetime(2026,9,30,14,tzinfo=timezone.utc)
    with pytest.raises(ValueError,match='expired'):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls


def test_sdk_half_day_market_is_closed_at_1300_new_york(sdk):
    sdk.state.half=True
    at=datetime(2026,9,30,17,tzinfo=timezone.utc)
    _, opened=bridge.market(sdk.quote,at)
    assert not opened


def test_sdk_context_hard_codes_server_paper_enforcement(sdk,monkeypatch):
    options=[]
    class OAuth:
        def __init__(self,client): assert client=='sdk-client'
        def build(self,callback):return 'opaque-oauth'
    class Config:
        @staticmethod
        def from_oauth(oauth,**kwargs):
            assert oauth=='opaque-oauth'; options.append(kwargs);return 'config'
    sdk.module.OAuthBuilder=OAuth; sdk.module.Config=Config
    sdk.module.AssetContext=sdk.module.TradeContext=sdk.module.QuoteContext=lambda config: config
    monkeypatch.setenv('LONGBRIDGE_PAPERTRADING','false')
    assert bridge.contexts('sdk-client') == ('config','config','config')
    assert options[0]['enable_papertrading'] is True
    assert options[0]['http_url']=='https://openapi.longbridge.com'


def test_sdk_background_context_refuses_new_authorization(sdk):
    class OAuth:
        def __init__(self,client):pass
        def build(self,callback):return callback('https://official.example/oauth')
    sdk.module.OAuthBuilder=OAuth
    sdk.module.Config=NS();sdk.module.AssetContext=sdk.module.TradeContext=sdk.module.QuoteContext=NS()
    with pytest.raises(ValueError,match='authorization required'):
        bridge.contexts('sdk-client')


def test_sdk_unsettled_proceeds_are_excluded(sdk):
    original=sdk.trade.account_balance
    def unsettled(currency):
        rows=original(currency); rows[0].cash_infos[0].settling_cash=Decimal('9000');return rows
    sdk.trade.account_balance=unsettled
    assert bridge.cash(sdk.trade)==('10000','1000')
    with pytest.raises(ValueError,match='settled cash'):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls
