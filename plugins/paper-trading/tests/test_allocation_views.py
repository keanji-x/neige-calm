"""Valuation history and the live SPY overview, driven through the production Allocation."""
from copy import deepcopy
from datetime import datetime, timedelta, timezone
from decimal import Decimal
import json
from pathlib import Path
import re

import jsonschema
import pytest

from paper_trading.allocation import Allocation
from paper_trading.allocation_report import tables
from .test_allocation import NOW, allocation_rig  # noqa: F401 - pytest fixture

ROOT = Path(__file__).parents[1]
SCHEMA = json.loads((ROOT.parents[1] / 'crates/calm-types/src/report_blocks/native_view.schema.json').read_text())


class Prescribed:
    """In-process broker records for long histories; Allocation still validates and reconciles them."""
    def __init__(self, raw):
        self.raw = raw

    def snapshot(self, since):
        return deepcopy(self.raw)

    def submit(self, request):
        raise AssertionError('history fixtures never submit')


def observe(r, quote_at, price, now=None):
    """Prescribe one broker quote and run one background-loop pass at a later clock."""
    state = r.read()
    state['snapshot']['quote'].update({'at': quote_at.isoformat(), 'price': price})
    r.write(state)
    r.app.clock = lambda: now or quote_at + timedelta(minutes=1)
    return r.step()


def filled(r):
    """60 SPY shares bought at 100 on 2026-09-30 (New York); 4000 USD cash remains."""
    r.plan(); r.execute(); r.publish()
    return r.fill('order-1', 60, '4000', 'buy-fill', 60)


def projected(r):
    """The publication input without a broker pass."""
    with r.app.ledger.session() as db:
        return r.app.projection(db)


def in_process(r):
    broker = Prescribed(r.read()['snapshot'])
    r.app = Allocation(r.root, r.config, broker, clock=r.app.clock)
    return broker


def advance(r, broker, quote_at, price):
    broker.raw['quote'].update({'at': quote_at.isoformat(), 'price': price})
    r.app.clock = lambda: quote_at + timedelta(minutes=1)
    return r.step()


def overview(state):
    return tables(state)['spy.overview']


def cell(view, identity):
    return next(c for row in view['rows'] for c in row['cells'] if c['id'] == identity)


def metrics(view):
    return {item['id']: item for item in cell(view, 'assets')['items']}


def valid(view):
    jsonschema.Draft202012Validator(SCHEMA).validate(view)
    json.dumps(view, allow_nan=False)
    return view


def test_valuation_sample_per_new_york_quote_date_latest_wins(allocation_rig):
    r = allocation_rig
    first = datetime(2026, 9, 29, 19, tzinfo=timezone.utc)   # 15:00 New York, 2026-09-29
    late = datetime(2026, 9, 30, 1, 30, tzinfo=timezone.utc)  # UTC date 09-30, New York still 09-29
    next_day = datetime(2026, 9, 30, 14, tzinfo=timezone.utc)
    observe(r, first, '100')
    observe(r, late, '101')
    state = observe(r, next_day, '102')
    assert state['valuations'] == [
        {'date': '2026-09-29', 'at': (late + timedelta(minutes=1)).isoformat(), 'equity_usd': '10000',
         'cash_usd': '10000', 'shares': 0, 'price': '101'},
        {'date': '2026-09-30', 'at': (next_day + timedelta(minutes=1)).isoformat(), 'equity_usd': '10000',
         'cash_usd': '10000', 'shares': 0, 'price': '102'}]
    # A refused broker observation is never adopted, so it records no sample.
    broken = r.read(); broken['snapshot']['identity']['account_no'] = 'OTHER'; r.write(broken)
    failed = observe(r, next_day + timedelta(hours=1), '150')
    assert 'identity' in failed['error'] and failed['valuations'] == state['valuations']
    r.restart()
    assert projected(r)['valuations'] == state['valuations']
    # Agents poll spy.status; the history is publication-only input.
    assert 'valuations' not in r.status() and set(projected(r)) == set(r.status()) | {'valuations'}


def test_previous_day_pnl_uses_a_strictly_earlier_new_york_date(allocation_rig):
    r = allocation_rig
    state = filled(r)
    assert [s['date'] for s in state['valuations']] == ['2026-09-30']
    view = valid(overview(state))
    items = metrics(view)
    # Several same-day observations exist, but none is a previous trading day: never invent zero.
    for key in ('previous', 'pnl', 'change'):
        assert items[key]['value']['state'] == 'unknown', key
    assert items['nav']['value'] == {'state': 'known', 'amount': 10000.0, 'unit': '$', 'decimals': 2,
                                     'signed': False, 'placement': 'prefix'}
    assert items['nav']['emphasis'] == 'primary' and 'SPY 市值 $6,000.00' in items['nav']['detail']
    observe(r, datetime(2026, 10, 1, 14, tzinfo=timezone.utc), '105')
    state = observe(r, datetime(2026, 10, 1, 18, tzinfo=timezone.utc), '104')
    items = metrics(valid(overview(state)))
    assert items['previous']['value']['amount'] == 10000.0 and '09.30' in items['previous']['label']
    assert items['pnl']['value'] == {'state': 'known', 'amount': 240.0, 'unit': '$', 'decimals': 2,
                                     'signed': True, 'placement': 'prefix'}
    assert items['pnl']['tone'] == 'positive'
    assert items['change']['value']['amount'] == 2.4 and items['change']['value']['unit'] == '%'
    holdings = cell(overview(state), 'holdings')['table']['rows']
    assert holdings[0] == {'name': 'SPY · 60 股', 'price': '104.00', 'value': '6,240.00', 'change': '+4.00%'}
    assert [s['value'] for s in cell(overview(state), 'weights')['slices']] == [6240.0, 4000.0]
    assert holdings[1]['name'] == '现金' and holdings[1]['value'] == '4,000.00'


def test_returns_and_benchmark_are_rebased_at_each_range_start(allocation_rig):
    r = allocation_rig
    filled(r)
    broker = in_process(r)
    for day in range(1, 46):
        state = advance(r, broker, NOW + timedelta(days=day), str(100 + day))
    view = valid(overview(state))
    chart = cell(view, 'nav-history')
    assert [d['id'] for d in chart['datasets']] == ['assets-all', 'returns-all', 'assets-1m', 'returns-1m']
    samples = state['valuations']
    for suffix, window in (('all', samples), ('1m', [s for s in samples if s['date'] >= '2026-10-15'])):
        returns = next(d for d in chart['datasets'] if d['id'] == 'returns-' + suffix)
        assets = next(d for d in chart['datasets'] if d['id'] == 'assets-' + suffix)
        assert [s['label'] for s in returns['series']] == ['组合', 'SPY 价格']
        assert [p['date'] for p in returns['points']] == [s['date'] for s in window]
        assert returns['points'][0]['values'] == [0.0, 0.0]
        start_equity = Decimal(4000) + 60 * Decimal(window[0]['price'])
        last = window[-1]
        expected = [float(round((Decimal(4000) + 60 * Decimal(last['price'])) / start_equity * 100 - 100, 4)),
                    float(round(Decimal(last['price']) / Decimal(window[0]['price']) * 100 - 100, 4))]
        assert returns['points'][-1]['values'] == expected
        assert assets['points'][-1]['values'] == [float(Decimal(4000) + 60 * Decimal(last['price']))]
    weights = cell(view, 'weight-history')
    assert [d['style'] for d in weights['datasets']] == ['stacked', 'line']
    point = weights['datasets'][0]['points'][-1]
    assert point['values'] == [float(round(Decimal(60 * 145) / Decimal(4000 + 60 * 145) * 100, 4)),
                               float(round(Decimal(4000) / Decimal(4000 + 60 * 145) * 100, 4))]
    short = valid(overview(state | {'valuations': samples[:3]}))
    assert [d['id'] for d in cell(short, 'nav-history')['datasets']] == ['assets-all', 'returns-all']


def test_decision_records_show_targets_states_and_actual_fills(allocation_rig):
    r = allocation_rig
    filled(r)
    r.plan(decision_id='allocation-2', target_spy_bps=3000)
    state = r.step()
    view = valid(overview(state))
    records = cell(view, 'decisions')
    [dataset] = records['datasets']
    first, second = dataset['items']
    assert [first['id'], second['id']] == ['allocation-2', 'allocation-1']
    assert first['badges'][0]['value'] == '等待执行' and first['disclosures'] == []
    facts = {f['label']: f['value'] for f in second['facts']}
    assert facts['目标 SPY 比例'] == '60.00%' and facts['订单'] == '买入 60 股'
    assert facts['委托编号'] == 'order-1' and facts['有效期至'].endswith('纽约时间')
    assert second['badges'][0] == {'label': '执行状态', 'value': '已成交', 'tone': 'positive'}
    sections = {s['label']: s['body'] for s in second['sections']}
    assert sections['理由'] == 'Captured research and market evidence support this allocation.'
    assert sections['来源'] == 'neige://source/research-1\nneige://source/market-1'
    [fill] = second['disclosures']
    assert '60 股' in fill['body'] and '$100' in fill['body'] and 'buy-fill' in fill['body']
    empty = cell(valid(overview(state | {'decisions': [], 'fills': []})), 'decisions')
    assert empty['datasets'][0]['items'] == [] and empty['emptyText']


def test_history_and_records_stay_bounded(allocation_rig):
    r = allocation_rig
    broker = in_process(r)
    for day in range(300):
        advance(r, broker, NOW + timedelta(days=day), '100')
    clock = NOW + timedelta(days=299, seconds=30)  # quote stays fresh for execution
    r.app.clock = lambda: clock
    for index in range(55):
        r.plan(decision_id=f'noop-{index:02}', target_spy_bps=0,
               valid_until=(clock + timedelta(hours=1)).isoformat())
        state = r.execute(f'noop-{index:02}')
    assert len(state['valuations']) == 260
    assert state['valuations'][-1]['date'] == (NOW + timedelta(days=299)).date().isoformat()
    view = valid(overview(state))
    assert len(cell(view, 'nav-history')['datasets'][0]['points']) == 260
    items = cell(view, 'decisions')['datasets'][0]['items']
    assert len(items) == 50 and items[0]['id'] == 'noop-54'


def test_overview_is_valid_before_reconciliation_and_after_errors(allocation_rig):
    r = allocation_rig
    state = projected(r)
    view = valid(overview(state))
    assert view['snapshot']['observedAt'] is None and view['snapshot']['producedAt'] is None
    assert all(item['value']['state'] == 'unknown' for item in metrics(view).values())
    assert cell(view, 'weights')['slices'] == [] and cell(view, 'holdings')['table']['rows'] == []
    assert overview(state) == view and state == projected(r)
    filled(r)
    broken = r.read(); broken['snapshot']['identity']['account_no'] = 'OTHER'; r.write(broken)
    state = r.step()
    view = valid(overview(state))
    assert '上次成功对账' in view['description']
    assert view['snapshot']['observedAt'] == int(NOW.timestamp() * 1000)
    assert [row['layout'] for row in view['rows']] == ['two-wide-end', 'three', 'one']
    assert [row['title'] for row in view['rows']] == ['01 · 组合表现', '02 · 资金投向', '03 · 调仓决策']


def test_spy_recipe_contract_matches_body_and_published_views(allocation_rig):
    text = (ROOT / 'spy-recipe.md').read_text()
    contract = json.loads(re.match(r'<!-- neige:contract (.*) -->\n', text).group(1))
    body = re.sub(r'<!--.*?-->', '', text, flags=re.S)
    headings = re.findall(r'^# (.+)$', body, flags=re.M)
    assert headings == [s['h1'] for s in contract['sections']]
    assert headings[0] == '组合概览' and headings[-1] == '来源与边界'
    assert [s['h1'] for s in contract['sections'] if s.get('omit_if_empty')] == ['待你定']
    sources = re.findall(r'"source":"neige://plugin/dev-neige-paper-trading/([^"]+)"', body)
    assert sources == ['spy.overview', 'spy.portfolio', 'spy.decisions', 'spy.fills']
    assert set(sources) == set(tables(projected(allocation_rig)))
    assert body.rstrip().endswith('仅作研究，不构成交易建议。')


class Filling(Prescribed):
    """Accepts one market order; the test prescribes its executions."""
    def submit(self, request):
        self.request = request
        return 'order-many'


def test_status_keeps_recent_fills_and_complete_decision_totals(allocation_rig):
    r = allocation_rig
    broker = Filling(r.read()['snapshot'] | {'cash_usd': '100000', 'available_cash_usd': '100000'})
    r.app = Allocation(r.root, r.config, broker, clock=r.app.clock)
    r.plan(); r.execute()
    quantity = broker.request['quantity']
    assert quantity > 200
    raw = broker.raw
    raw['orders'] = [broker.request | {'order_id': 'order-many', 'quantity': str(quantity),
                                       'executed_quantity': str(quantity), 'status': 'Filled'}]
    # Trade IDs sort opposite to execution time.
    raw['fills'] = [{'trade_id': f'fill-{quantity - i:04}', 'order_id': 'order-many', 'symbol': 'SPY.US',
                     'quantity': '1', 'price': '100', 'time': (NOW + timedelta(seconds=i)).isoformat()}
                    for i in range(quantity)]
    raw['shares'] = raw['available_shares'] = quantity
    raw['cash_usd'] = raw['available_cash_usd'] = str(100000 - 100 * quantity)
    state = r.step()
    assert state['error'] is None
    times = [f['time'] for f in state['fills']]
    assert len(times) == 200 and times == sorted(times)
    assert times[-1] == (NOW + timedelta(seconds=quantity - 1)).isoformat()
    [record] = cell(valid(overview(state)), 'decisions')['datasets'][0]['items']
    assert f'已成交 {quantity} 股' in record['summary']


def test_snapshot_identity_covers_the_description(allocation_rig):
    state = filled(allocation_rig)
    assert overview(state)['snapshot']['id'] != overview(state | {'error': 'broker unavailable'})['snapshot']['id']
