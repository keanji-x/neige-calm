"""Valuation history and the live SPY data units, driven through the production Allocation."""
from copy import deepcopy
from datetime import datetime, timedelta, timezone
from decimal import Decimal
import json
from pathlib import Path
import re

import jsonschema
import pytest

from paper_trading.allocation import Allocation
from paper_trading.allocation_views import units
from . import recipe
from .test_allocation import NOW, allocation_rig  # noqa: F401 - pytest fixture

ROOT = Path(__file__).parents[1]
SCHEMA = json.loads((ROOT.parents[1] / 'crates/calm-types/src/report_blocks/native_view.schema.json').read_text())
# The generated contract's own DataUnit definition: the envelope a live slot resolves to.
UNIT_SCHEMA = {'$schema': SCHEMA['$schema'], '$defs': SCHEMA['$defs'], '$ref': '#/$defs/DataUnit'}
PLUGIN = json.loads((ROOT / 'manifest.json').read_text())['id']
MAX_UNIT_BYTES = 4 * 1024 * 1024  # calm-types MAX_LIVE_UNIT_BYTES: the per-unit read cap


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


def valid(unit):
    jsonschema.Draft202012Validator(UNIT_SCHEMA).validate(unit)
    encoded = json.dumps(unit, ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode()
    assert len(encoded) <= MAX_UNIT_BYTES
    return unit


def cells(state):
    """Every published unit, each validated as a data unit, by overlay kind."""
    return {kind: valid(unit)['cell'] for kind, unit in units(state).items()}


def metrics(cell):
    return {item['id']: item for item in cell['items']}


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
    items = metrics(cells(state)['spy.nav'])
    # Several same-day observations exist, but none is a previous trading day: never invent zero.
    for key in ('previous', 'pnl', 'change'):
        assert items[key]['value']['state'] == 'unknown', key
    assert items['nav']['value'] == {'state': 'known', 'amount': 10000.0, 'unit': '$', 'decimals': 2,
                                     'signed': False, 'placement': 'prefix'}
    assert items['nav']['emphasis'] == 'primary' and 'SPY 市值 $6,000.00' in items['nav']['detail']
    observe(r, datetime(2026, 10, 1, 14, tzinfo=timezone.utc), '105')
    state = observe(r, datetime(2026, 10, 1, 18, tzinfo=timezone.utc), '104')
    published = cells(state)
    items = metrics(published['spy.nav'])
    assert items['previous']['value']['amount'] == 10000.0 and '09.30' in items['previous']['label']
    assert items['pnl']['value'] == {'state': 'known', 'amount': 240.0, 'unit': '$', 'decimals': 2,
                                     'signed': True, 'placement': 'prefix'}
    assert items['pnl']['tone'] == 'positive'
    assert items['change']['value']['amount'] == 2.4 and items['change']['value']['unit'] == '%'
    holdings = published['spy.holdings']['table']['rows']
    assert holdings[0] == {'name': 'SPY · 60 股', 'price': '104.00', 'value': '6,240.00', 'change': '+4.00%'}
    assert [s['value'] for s in published['spy.weights']['slices']] == [6240.0, 4000.0]
    # The exact reconciled ratio at two decimals: 6240 / 10240, not the whole-dollar slices' rounding.
    assert state['snapshot']['actual_spy_bps'].startswith('6093.75')
    assert published['spy.holdings']['table']['caption'].startswith('实际 SPY 比例 60.94%（按市值 / 总资产）· 行情时间')
    assert holdings[1]['name'] == '现金' and holdings[1]['value'] == '4,000.00'


def test_returns_and_benchmark_are_rebased_at_each_range_start(allocation_rig):
    r = allocation_rig
    filled(r)
    broker = in_process(r)
    for day in range(1, 46):
        state = advance(r, broker, NOW + timedelta(days=day), str(100 + day))
    published = cells(state)
    chart = published['spy.nav_history']
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
    weights = published['spy.weight_history']
    assert [d['style'] for d in weights['datasets']] == ['stacked', 'line']
    point = weights['datasets'][0]['points'][-1]
    assert point['values'] == [float(round(Decimal(60 * 145) / Decimal(4000 + 60 * 145) * 100, 4)),
                               float(round(Decimal(4000) / Decimal(4000 + 60 * 145) * 100, 4))]
    short = cells(state | {'valuations': samples[:3]})
    assert [d['id'] for d in short['spy.nav_history']['datasets']] == ['assets-all', 'returns-all']


def test_decision_records_show_targets_states_and_actual_fills(allocation_rig):
    r = allocation_rig
    filled(r)
    r.plan(decision_id='allocation-2', target_spy_bps=3000)
    state = r.step()
    records = cells(state)['spy.decision_log']
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
    empty = cells(state | {'decisions': [], 'fills': []})
    assert empty['spy.decision_log']['datasets'][0]['items'] == [] and empty['spy.decision_log']['emptyText']
    assert empty['spy.fill_log']['table']['rows'] == []
    assert empty['spy.fill_log']['table']['caption'].startswith('尚无成交记录；')


def test_history_and_records_stay_bounded(allocation_rig):
    r = allocation_rig
    broker = in_process(r)
    for day in range(300):
        advance(r, broker, NOW + timedelta(days=day), '100')
    clock = NOW + timedelta(days=299, seconds=30)  # quote stays fresh for execution
    r.app.clock = lambda: clock
    # The largest accepted plan: a 6000-character rationale in 3-byte UTF-8 and 20 maximal sources.
    rationale = '理' * 6000
    refs = [f'neige://source/{index:02}-' + 'r' * 494 for index in range(20)]
    for index in range(55):
        r.plan(decision_id=f'noop-{index:02}', target_spy_bps=0, rationale=rationale, source_refs=refs,
               valid_until=(clock + timedelta(hours=1)).isoformat())
        state = r.execute(f'noop-{index:02}')
    assert len(state['valuations']) == 260
    assert state['valuations'][-1]['date'] == (NOW + timedelta(days=299)).date().isoformat()
    published = cells(state)  # each unit within the 4 MiB per-unit read cap
    assert len(published['spy.nav_history']['datasets'][0]['points']) == 260
    assert all(len(d['points']) == 260 for d in published['spy.weight_history']['datasets'])
    items = published['spy.decision_log']['datasets'][0]['items']
    assert len(items) == 50 and items[0]['id'] == 'noop-54'
    sections = {s['label']: s['body'] for s in items[0]['sections']}
    assert sections['理由'] == rationale and len(sections['来源']) == 8000


def test_units_are_valid_before_reconciliation_and_after_errors(allocation_rig):
    r = allocation_rig
    state = projected(r)
    published = units(state)
    assert set(cells(state)) == set(published)
    assert all(u['snapshot']['observedAt'] is None and u['snapshot']['producedAt'] is None for u in published.values())
    assert all(item['value']['state'] == 'unknown' for item in metrics(published['spy.nav']['cell']).values())
    assert published['spy.weights']['cell']['slices'] == [] and published['spy.holdings']['cell']['table']['rows'] == []
    account = metrics(published['spy.account']['cell'])
    for key in ('reconciliation', 'quote', 'available-cash'):
        assert account[key]['value'] == {'state': 'unknown', 'reason': '尚未完成账户对账'}, key
    assert account['max-order']['value']['amount'] == 100.0 and account['cash-buffer']['value']['amount'] == 2.0
    assert units(state) == published and state == projected(r)
    filled(r)
    broken = r.read(); broken['snapshot']['identity']['account_no'] = 'OTHER'; r.write(broken)
    state = r.step()
    published = units(state)
    reconciliation = metrics(cells(state)['spy.account'])['reconciliation']
    # A failed reconciliation is publisher meaning: a negative account item, not a platform staleness rule.
    assert reconciliation['tone'] == 'negative' and reconciliation['value'] == {'state': 'text', 'text': state['error']}
    assert reconciliation['detail'].startswith('最近一次对账失败；当前显示 2026-09-30 11:00 纽约时间')
    assert all(u['snapshot']['observedAt'] == int(NOW.timestamp() * 1000) for u in published.values())


def test_spy_recipe_contract_matches_body_and_published_units(allocation_rig):
    text = (ROOT / 'spy-recipe.md').read_text()
    contract = json.loads(re.match(r'<!-- neige:contract (.*) -->\n', text).group(1))
    body = re.sub(r'<!--.*?-->', '', text, flags=re.S)
    headings = re.findall(r'^# (.+)$', body, flags=re.M)
    assert headings == [s['h1'] for s in contract['sections']]
    assert headings == ['组合表现', '仓位配置', '调仓决策']
    # Execution tasks append to the final section; routine research must leave all views and tasks intact.
    steps = re.findall(r'^- (?:Pre-market|Post-close|Weekly):.*$', text, flags=re.M)
    assert len(steps) == 3 and all('reply in the conversation' in step for step in steps)
    assert 'No step rewrites 组合表现, 仓位配置 or 调仓决策' in text
    assert 'A user edit of this Report requests no step.' in text
    assert not any(s.get('omit_if_empty') for s in contract['sections'])
    # Account mode is recipe context, never unit data (see the example's marker rule).
    assert '长桥官方模拟账户 · SPY／现金' in text
    views = recipe.views(body)
    assert len(views) == 3
    for view in views:
        jsonschema.Draft202012Validator(SCHEMA).validate(view)
    slots = [c for view in views for row in view['rows'] for c in row['cells']]
    assert all(c['kind'] == 'live' and c['source'].startswith(f'neige://plugin/{PLUGIN}/') for c in slots)
    expects = {c['source'].rsplit('/', 1)[1]: c['expects'] for c in slots}
    # Every plugin source in the body is one of these slots: no live table names a retired kind.
    assert re.findall(rf'"source":"neige://plugin/{PLUGIN}/([^"]+)"', body) == list(expects)
    published = units(projected(allocation_rig))
    assert len(expects) == len(slots) and set(expects) == set(published)
    for kind, unit in published.items():
        assert unit['cell']['kind'] == expects[kind], kind
    assert 'ending 仅作研究，不构成交易建议。' in text
    assert "Persist the decision's sourced reasoning" in text
    # The 总览 keeps one link to its research Track and asks it by mail; it never researches itself.
    assert re.findall(r'^- Pre-market decision, on a mail wake:', text, flags=re.M)
    assert '`- [SPY 研究](neige://wave/<track_id>)`' in text and 'idempotency_key "spy-research"' in text
    assert 'neige_mail_send' in text and 'Never research yourself.' in text
    # The decision reads the research reply's sender and date, and cites it as the 总览's own source.
    assert 'neige --json mail cat <mail_id>' in text and 'its track_id is the research Track' in text
    assert 'its summary starts with "SPY 盘前研究 <today\'s New York date>"' in text
    assert "Capture the reply as this Track's own source with neige_source_capture (provenance manual" in text
    assert 'The pre-market decision reads spy.status without refreshing' in text


def test_spy_research_recipe_is_a_report_without_live_views_or_trading():
    text = (ROOT / 'spy-research-recipe.md').read_text()
    contract = json.loads(re.match(r'<!-- neige:contract (.*) -->\n', text).group(1))
    body = re.sub(r'<!--.*?-->', '', text, flags=re.S)
    headings = re.findall(r'^# (.+)$', body, flags=re.M)
    assert headings == [s['h1'] for s in contract['sections']]
    assert headings == ['结论', '核心逻辑', '关键数据', '风险与证伪', '催化剂与跟踪', '来源与边界']
    assert recipe.views(text) == [] and 'neige://plugin/' not in text
    # It links back to the 总览, answers by reply mail and never trades.
    assert '`[SPY 总览](neige://wave/<总览 track_id>)`' in text
    assert 'neige --json mail cat <mail_id>' in text and 'whose track_id is the 总览 Track' in text
    assert text.count('neige_mail_send (mail_id = the request)') == 2
    assert 'summary starting with the request\'s "SPY 盘前研究 YYYY-MM-DD"' in text
    assert 'Never trade, call spy.* tools, add Calendar entries or declare tasks' in text
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
    published = cells(state)
    [record] = published['spy.decision_log']['datasets'][0]['items']
    assert f'已成交 {quantity} 股' in record['summary']
    # A decision lists at most 20 fills; the fill log carries every fill in the bounded status list.
    assert len(record['disclosures']) == 20
    fills = published['spy.fill_log']['table']
    assert [row['trade_id'] for row in fills['rows']] == [f['trade_id'] for f in reversed(state['fills'])]
    assert fills['rows'][0]['time'].endswith('纽约时间') and fills['caption'].startswith('最近 200 笔成交')


def test_snapshot_identity_covers_each_unit_cell(allocation_rig):
    state = filled(allocation_rig)
    before, after = units(state), units(state | {'error': 'broker unavailable'})
    assert before['spy.account']['snapshot']['id'] != after['spy.account']['snapshot']['id']
    # Identity is per unit: a cell the error does not change keeps its identity.
    assert before['spy.nav']['snapshot']['id'] == after['spy.nav']['snapshot']['id']
