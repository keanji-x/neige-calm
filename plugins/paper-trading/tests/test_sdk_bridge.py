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
               quote_at=NOW, quote_status='Normal', channel='lb_papertrading', account='PAPER123', orders=[], half=False)

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
                       trade_status='TradeStatus.'+state.quote_status)]

        def trading_days(self, market, begin, end):
            assert begin == end
            return NS(trading_days=[] if state.half else [begin], half_trading_days=[begin] if state.half else [])

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
    class Clock(datetime):
        @classmethod
        def now(cls, zone=None): return NOW
    monkeypatch.setattr(bridge, 'datetime', Clock)
    request = {'symbol':'SPY.US','side':'Buy','quantity':60,'order_type':'MO','time_in_force':'Day',
               'outside_rth':'RTH_ONLY','client_request_id':'a' * 64,'remark':'nc-spy-' + 'a' * 32,'basis_shares':0}
    return NS(asset=Asset(), trade=Trade(), quote=Quote(), state=state, calls=calls,
              module=module, request=request,
              policy={'cash_buffer_bps':200,'max_order_bps':10000,'quote_max_age_seconds':60})


def test_sdk_submit_is_cash_funded_spy_market_order(sdk):
    result = bridge.submit(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', sdk.request, sdk.policy)
    assert result == {'order_id':'sdk-order'}
    assert sdk.calls == [{'symbol':'SPY.US','order_type':'MO','side':'Buy','submitted_quantity':Decimal(60),
                         'time_in_force':'Day','outside_rth':'RTH_ONLY',
                         'remark':'nc-spy-'+'a'*32, 'client_request_id':'a'*64}]


@pytest.mark.parametrize('field,value,match', [('channel','lb','paper-account'), ('account','OTHER','configured paper')])
def test_sdk_identity_fence(sdk, field, value, match):
    setattr(sdk.state,field,value)
    with pytest.raises(bridge.OrderNotSubmitted):
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
    with pytest.raises(bridge.OrderNotSubmitted):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls


def test_sdk_holdings_recheck(sdk):
    sdk.state.shares=sdk.state.available=1
    with pytest.raises(bridge.OrderNotSubmitted):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls


def test_sdk_freshness_recheck(sdk):
    sdk.state.quote_at=datetime(2026,9,30,14,tzinfo=timezone.utc)
    with pytest.raises(bridge.OrderNotSubmitted):
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
    with pytest.raises(bridge.OrderNotSubmitted):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls



def test_sdk_half_only_calendar_opens_in_the_morning(sdk):
    sdk.state.half=True
    _,opened=bridge.market(sdk.quote,NOW)
    assert opened



def test_sdk_quote_created_during_network_read_is_fresh(sdk,monkeypatch):
    from datetime import timedelta
    clock={'now':NOW}
    class Clock(datetime):
        @classmethod
        def now(cls, zone=None): return clock['now']
    monkeypatch.setattr(bridge,'datetime',Clock)
    original=sdk.quote.quote
    def after_network(symbols):
        clock['now']=NOW+timedelta(seconds=1)
        sdk.state.quote_at=clock['now']
        return original(symbols)
    sdk.quote.quote=after_network
    assert bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)=={'order_id':'sdk-order'}
    assert len(sdk.calls)==1


def test_sdk_exception_after_write_started_is_not_not_submitted(sdk):
    def lost_response(**kwargs):
        sdk.calls.append(kwargs)
        raise RuntimeError('response lost after acceptance')
    sdk.trade.submit_order=lost_response
    with pytest.raises(RuntimeError,match='response lost'):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert len(sdk.calls)==1



def test_sdk_run_emits_exact_not_submitted_proof(monkeypatch,capsys):
    def fail_before_write():raise bridge.OrderNotSubmitted('private diagnostic')
    monkeypatch.setattr(bridge,'main',fail_before_write)
    bridge.run()
    result=capsys.readouterr()
    assert json.loads(result.out)=={'status':'not_submitted'}
    assert 'private diagnostic' not in result.err+result.out


def test_sdk_run_does_not_claim_proof_after_ambiguous_failure(monkeypatch,capsys):
    def fail_unknown():raise RuntimeError('private diagnostic')
    monkeypatch.setattr(bridge,'main',fail_unknown)
    with pytest.raises(SystemExit) as error:bridge.run()
    result=capsys.readouterr()
    assert error.value.code==1 and result.out==''
    assert 'private diagnostic' not in result.err



def test_sdk_non_normal_quote_is_not_submitted(sdk):
    sdk.state.quote_status='Suspended'
    with pytest.raises(bridge.OrderNotSubmitted):
        bridge.submit(sdk.asset,sdk.trade,sdk.quote,'PAPER123',sdk.request,sdk.policy)
    assert not sdk.calls


def test_sdk_context_uses_explicit_official_cn_access_point(sdk):
    options=[]
    class OAuth:
        def __init__(self,client):assert client=='sdk-client'
        def build(self,callback):return 'cached-oauth'
    class Config:
        @staticmethod
        def from_oauth(oauth,**kwargs):options.append(kwargs);return 'config'
    sdk.module.OAuthBuilder=OAuth;sdk.module.Config=Config
    sdk.module.AssetContext=sdk.module.TradeContext=sdk.module.QuoteContext=lambda config:config
    assert bridge.contexts('sdk-client',access_region='cn')==('config','config','config')
    assert options[0]['http_url']=='https://openapi.longbridge.cn'
    assert options[0]['quote_ws_url']=='wss://openapi-quote.longbridge.cn/v2'
    assert options[0]['enable_papertrading'] is True


def test_sdk_native_local_datetime_preserves_epoch(monkeypatch):
    import os,time
    from datetime import datetime,timezone
    old=os.environ.get('TZ')
    try:
        monkeypatch.setenv('TZ','Asia/Shanghai');time.tzset()
        expected=datetime(2026,10,1,1,2,3,tzinfo=timezone.utc)
        native=datetime.fromtimestamp(expected.timestamp())
        assert native.tzinfo is None
        assert bridge.utc(native)==expected.isoformat()
    finally:
        if old is None:os.environ.pop('TZ',None)
        else:os.environ['TZ']=old
        time.tzset()
