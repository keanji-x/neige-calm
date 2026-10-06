"""The multi-symbol official-SDK adapter, against prescribed SDK response objects (no network)."""
from datetime import datetime, timedelta, timezone
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
               channel='lb_papertrading', account='PAPER123', orders=[], history=[], currency='USD',
               history_queries=[], calendar='trading')

    def listed(rows, status):
        wanted = None if status is None else {bridge.enum(s) for s in status}
        return [o for o in rows if wanted is None or bridge.enum(o.status) in wanted]

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

        def today_orders(self, status=None):
            return listed(state.orders, status)

        def history_orders(self, status=None, market=None, start_at=None, end_at=None):
            state.history_queries.append({'status': status, 'market': market, 'start_at': start_at, 'end_at': end_at})
            return listed([o for o in state.history if start_at is None or o.created_at >= start_at], status)

        def order_detail(self, order_id):
            return next(o for o in state.orders + state.history if o.order_id == order_id)

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
            return NS(trading_days=[begin] if state.calendar == 'trading' else [],
                      half_trading_days=[begin] if state.calendar == 'half' else [])

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
    statuses = bridge.ACTIVE + ('Filled', 'Canceled', 'Rejected', 'Expired', 'PartialWithdrawal')
    module = NS(StatementType=NS(Daily='daily'), Market=NS(US='US'), OrderType=NS(MO='MO'),
                OrderStatus=NS(**{name: f'OrderStatus.{name}' for name in statuses}),
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


@pytest.mark.parametrize('calendar,trading_day,half_day,close', [
    ('trading', True, False, '2026-09-30T20:00:00+00:00'),
    ('half', True, True, '2026-09-30T17:00:00+00:00'),
    ('holiday', False, False, '2026-09-30T20:00:00+00:00')])
def test_snapshot_session_carries_the_broker_calendar_for_its_date(sdk, calendar, trading_day, half_day, close):
    sdk.state.calendar = calendar
    raw = bridge.snapshot(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', {'since': None, 'symbols': []})
    assert {k: raw['session'][k] for k in ('calendar_date', 'trading_day', 'half_day', 'regular_close_at')} == {
        'calendar_date': '2026-09-30', 'trading_day': trading_day, 'half_day': half_day, 'regular_close_at': close}
    assert raw['market_open'] is (calendar != 'holiday')


def test_snapshot_crossing_midnight_keeps_the_queried_calendar_date(sdk, monkeypatch):
    from invest.reconcile import validate_snapshot
    # Sunday 23:59:59 New York at the SDK read; the App validates two seconds later, on Monday.
    read_at = datetime(2026, 10, 5, 3, 59, 59, tzinfo=timezone.utc)
    validated_at = datetime(2026, 10, 5, 4, 0, 1, tzinfo=timezone.utc)
    class Clock(datetime):
        @classmethod
        def now(cls, zone=None): return read_at
    monkeypatch.setattr(bridge, 'datetime', Clock)
    sdk.state.quote_at, sdk.state.calendar = read_at, 'holiday'
    raw = json.loads(json.dumps(bridge.snapshot(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', {'since': None, 'symbols': []})))
    snapshot = validate_snapshot(raw, NS(account_no='PAPER123'), validated_at)
    assert snapshot['at'] == validated_at.isoformat()
    assert (snapshot['calendar_date'], snapshot['trading_day']) == ('2026-10-04', False)
    assert snapshot['regular_close_at'] == '2026-10-04T20:00:00+00:00'


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


def gtc(status='New', days_ago=1):
    """A long-lived order placed outside invest on an earlier day."""
    return NS(order_id='gtc-1', symbol='QQQ.US', side='OrderSide.Buy', order_type='OrderType.LO', quantity=Decimal(1),
              executed_quantity=Decimal(0), status=f'OrderStatus.{status}', remark='',
              time_in_force='TimeInForceType.GoodTilCanceled', outside_rth='OutsideRTH.RTHOnly',
              created_at=NOW - timedelta(days=days_ago))


def test_snapshot_discovers_active_orders_from_earlier_days(sdk):
    sdk.state.history = [gtc()]
    raw = bridge.snapshot(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', {'since': None, 'symbols': []})
    assert [(o['order_id'], o['status']) for o in raw['orders']] == [('gtc-1', 'New')]
    # The query is account-wide, US, active statuses only, over a bounded window.
    [query] = sdk.state.history_queries
    assert query['market'] == 'US' and {bridge.enum(s) for s in query['status']} == set(bridge.ACTIVE)
    assert query['end_at'] == NOW and query['start_at'] == NOW - bridge.ACTIVE_LOOKBACK
    assert bridge.ACTIVE_LOOKBACK >= timedelta(days=400)


def test_preflight_refuses_beside_an_active_order_from_an_earlier_day(sdk):
    sdk.state.history = [gtc()]
    with pytest.raises(bridge.OrderNotSubmitted):
        submit(sdk)
    sdk.state.history = [gtc(status='Canceled')]
    assert submit(sdk) == {'order_id': 'sdk-order'}
    assert len(sdk.calls) == 1


def test_historical_active_order_blocks_reconciliation_and_submit(sdk, rig):
    """The production Portfolio over the real bridge: an unfilled GTC order placed yesterday."""
    sdk.state.positions = {}

    class SdkBroker:
        def snapshot(self, since, symbols):
            raw = bridge.snapshot(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', {'since': since, 'symbols': symbols})
            return json.loads(json.dumps(raw, default=str))

        def submit(self, request):
            return bridge.submit(sdk.asset, sdk.trade, sdk.quote, 'PAPER123', request, sdk.policy)['order_id']

    rig.broker = SdkBroker(); rig.restart()
    rig.watch('US:CCC')
    rig.decide({'US:CCC': 500})
    sdk.state.history = [gtc()]
    state = rig.execute()
    assert 'unowned active broker order' in state['error'] and sdk.calls == []
    assert state['decisions'][0]['state'] == 'requested'
    sdk.state.history = [gtc(status='Canceled')]
    state = rig.step()
    assert state['error'] is None and [c['symbol'] for c in sdk.calls] == ['CCC.US']


def test_unknown_order_status_today_blocks_preflight(sdk):
    sdk.state.orders = [gtc(status='Unknown', days_ago=0)]
    with pytest.raises(bridge.OrderNotSubmitted):
        submit(sdk)
    assert sdk.calls == []


def candle(day, close, hour=0):
    """A daily candlestick stamped at New York midnight of its session (plus `hour`)."""
    stamp = datetime.combine(day, datetime.min.time(), tzinfo=bridge.NY) + timedelta(hours=hour)
    return NS(timestamp=stamp, open=Decimal('1'), high=Decimal('2'), low=Decimal('0.5'), close=Decimal(close),
              volume=10)


@pytest.fixture
def quote(monkeypatch):
    from datetime import date
    module = NS(Period=NS(Day='Period.Day'), AdjustType=NS(ForwardAdjust='AdjustType.ForwardAdjust'),
                TradeSessions=NS(Intraday='TradeSessions.Intraday'))
    monkeypatch.setitem(sys.modules, 'longbridge.openapi', module)
    state = NS(calls=[], days={}, latest={}, rows=None)

    class Quote:
        def candlesticks(self, symbol, period, count, adjust, sessions):
            state.calls.append(('probe', symbol, period, count, adjust, sessions))
            if symbol not in state.latest:
                raise RuntimeError('private SDK diagnostic')
            return [candle(state.latest[symbol], '9')]

        def history_candlesticks_by_date(self, symbol, period, adjust, start, end, sessions):
            state.calls.append(('window', symbol, start, end, period, adjust, sessions))
            if state.rows is not None:
                return state.rows
            return [candle(d, '5') for d in state.days.get(symbol, []) if start <= d <= end]
    return NS(quote=Quote(), state=state, date=date)


def test_series_probes_then_fetches_the_window_in_bounded_requests(quote):
    d = quote.date
    quote.state.latest = {'AAA.US': d(2026, 9, 11)}
    quote.state.days = {'AAA.US': [d(2021, 6, 1), d(2024, 1, 2), d(2026, 9, 10)]}
    answer = bridge.series(quote.quote, {'symbols': ['AAA.US'], 'start': '2021-06-01', 'end': '2026-09-10'})
    assert answer == {'series': {'AAA.US': {'complete_through': '2026-09-11', 'bars': [
        [day.isoformat(), '1', '2', '0.5', '5', '10'] for day in quote.state.days['AAA.US']]}}}
    probe, *windows = quote.state.calls
    assert probe == ('probe', 'AAA.US', 'Period.Day', 1, 'AdjustType.ForwardAdjust', 'TradeSessions.Intraday')
    # Contiguous, non-overlapping requests of at most 1000 calendar days covering the whole window.
    assert windows[0][2] == d(2021, 6, 1) and windows[-1][3] == d(2026, 9, 10)
    assert all((w[3] - w[2]).days < bridge.SERIES_CHUNK_DAYS for w in windows)
    assert all(a[3] + timedelta(days=1) == b[2] for a, b in zip(windows, windows[1:]))
    assert {w[4:] for w in windows} == {('Period.Day', 'AdjustType.ForwardAdjust', 'TradeSessions.Intraday')}


def test_series_dates_bars_by_their_new_york_session(quote):
    d = quote.date
    quote.state.latest = {'AAA.US': d(2026, 9, 11)}
    # 16:00 New York is 20:00 UTC: still the same session date.
    quote.state.rows = [candle(d(2026, 9, 10), '5', hour=16)]
    answer = bridge.series(quote.quote, {'symbols': ['AAA.US'], 'start': '2026-09-01', 'end': '2026-09-10'})
    assert answer['series']['AAA.US']['bars'][0][0] == '2026-09-10'


def test_series_failures_stay_per_symbol_and_never_quote_the_sdk(quote):
    d = quote.date
    # CCC's probe raises: its own entry fails, AAA still answers.
    quote.state.latest = {'AAA.US': d(2026, 9, 11)}
    quote.state.rows = [candle(d(2026, 9, 10), '5')]
    answer = bridge.series(quote.quote, {'symbols': ['CCC.US', 'AAA.US'], 'start': '2026-09-01', 'end': '2026-09-10'})
    assert answer['series']['CCC.US'] == {'error': 'official SDK candlestick query failed'}
    assert answer['series']['AAA.US']['complete_through'] == '2026-09-11'
    assert 'private' not in json.dumps(answer)
    # A window answering the row ceiling may be truncated, and a bar outside its window is refused.
    quote.state.rows = [candle(d(2026, 9, 10), '5')] * bridge.SERIES_ROW_CEILING
    answer = bridge.series(quote.quote, {'symbols': ['AAA.US'], 'start': '2026-09-01', 'end': '2026-09-10'})
    assert 'error' in answer['series']['AAA.US']
    quote.state.rows = [candle(d(2026, 8, 1), '5')]
    answer = bridge.series(quote.quote, {'symbols': ['AAA.US'], 'start': '2026-09-01', 'end': '2026-09-10'})
    assert 'error' in answer['series']['AAA.US']
    with pytest.raises(ValueError):
        bridge.series(quote.quote, {'symbols': ['700.HK'], 'start': '2026-09-01', 'end': '2026-09-10'})
    with pytest.raises(ValueError):
        bridge.series(quote.quote, {'symbols': [f'A{i}.US' for i in range(9)], 'start': '2026-09-01',
                                    'end': '2026-09-10'})
