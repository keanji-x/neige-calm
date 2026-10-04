"""Portfolio data units: valid against the kernel's contract, and aggregated without losing value."""
from datetime import datetime, timedelta, timezone
from decimal import Decimal
import json

import jsonschema

from account import SimulatedAccount
from invest.views import units
from recipe import unit_kinds
from rig import NOW, ROOT

SCHEMA = json.loads((ROOT.parents[1] / 'crates/calm-types/src/report_blocks/native_view.schema.json').read_text())
UNIT_SCHEMA = {'$schema': SCHEMA['$schema'], '$defs': SCHEMA['$defs'], '$ref': '#/$defs/DataUnit'}
MAX_UNIT_BYTES = 4 * 1024 * 1024  # calm-types MAX_LIVE_UNIT_BYTES


def semantic(cell):
    """The checks `native_view.rs` adds to the schema: unique ids, increasing dates, point width,
    complete non-negative stacked values. (No Python hook reaches the kernel's `validate_unit`.)"""
    def unique(ids):
        assert len(ids) == len(set(ids)), ids
    if cell['kind'] == 'metrics':
        unique([i['id'] for i in cell['items']])
    if cell['kind'] == 'distribution':
        unique([s['id'] for s in cell['slices']])
    if cell['kind'] == 'time-series':
        unique([d['id'] for d in cell['datasets']])
        for data in cell['datasets']:
            unique([s['id'] for s in data['series']])
            dates = [p['date'] for p in data['points']]
            assert dates == sorted(set(dates)), dates
            for point in data['points']:
                assert len(point['values']) == len(data['series'])
                if data['style'] == 'stacked':
                    assert all(v is not None and v >= 0 for v in point['values'])
    if cell['kind'] == 'records':
        unique([d['id'] for d in cell['datasets']])
        for data in cell['datasets']:
            unique([i['id'] for i in data['items']])
            for item in data['items']:
                unique([e['id'] for e in item['disclosures']])


def valid(unit):
    jsonschema.Draft202012Validator(UNIT_SCHEMA).validate(unit)
    semantic(unit['cell'])
    assert len(json.dumps(unit, ensure_ascii=False, separators=(',', ':')).encode()) <= MAX_UNIT_BYTES
    return unit


def exact(value):
    """A published float back to the decimal it was written from."""
    return Decimal(repr(value))


def cells(state):
    published = units(state)
    assert set(published) == unit_kinds()
    return {kind: valid(unit)['cell'] for kind, unit in published.items()}


def simulated(r, cash, prices, split=False):
    account = SimulatedAccount(cash, prices, lambda: r.clock(), split)
    r.broker = account
    r.restart()
    return account


def test_projections_conserve_value(rig):
    r = rig
    symbols = [f'US:S{i:02d}' for i in range(40)]
    r.configure(max_held=40, max_watched=1)
    account = simulated(r, '1000000', {f'S{i:02d}.US': 10 + i for i in range(40)})
    r.watch(*symbols)
    weights = {s: 100 + 5 * i for i, s in enumerate(symbols)}
    r.decide(weights); r.request()
    for _ in range(45):
        state = r.step()
        if state['decisions'][0]['state'] == 'done':
            break
    assert state['decisions'][0]['state'] == 'done' and len(account.orders) == 40
    for day in (1, 2):
        r.clock = lambda day=day: NOW + timedelta(days=day)
        account.prices = {k: v + 1 for k, v in account.prices.items()}
        state = r.step()
    assert state['error'] is None and len(state['valuations']) == 3
    published = cells(state)

    snapshot = state['snapshot']
    values = {s: Decimal(p['value_usd']) for s, p in snapshot['positions'].items()}
    ranked = sorted(values, key=lambda s: (-values[s], s))
    assert len(ranked) == 40

    # Weights: top 10 + 其他 + 现金, summing to equity; 其他 is exactly what it replaces.
    slices = {s['id']: s for s in published['portfolio.weights']['slices']}
    assert len(slices) == 12
    assert [s['label'] for s in published['portfolio.weights']['slices'][:10]] == ranked[:10]
    assert slices['other']['value'] == float(sum(values[s] for s in ranked[10:]))
    assert slices['cash']['value'] == float(Decimal(snapshot['cash_usd']))
    assert sum(s['value'] for s in slices.values()) == float(Decimal(snapshot['equity_usd']))

    # Weight history: top 4 now + 其他 + 现金; every point sums to 100%.
    shown = set(ranked[:4])
    for dataset in published['portfolio.weight_history']['datasets']:
        assert [s['label'] for s in dataset['series'][:4]] == ranked[:4]
        assert [s['id'] for s in dataset['series'][4:]] == ['other', 'cash']
        assert len(dataset['points']) == 3
        for point, sample in zip(dataset['points'], state['valuations']):
            equity = Decimal(sample['equity_usd'])
            omitted = sum(Decimal(p['shares']) * Decimal(p['price'])
                          for s, p in sample['positions'].items() if s not in shown)
            assert sum(exact(v) for v in point['values']) == 100
            assert abs(exact(point['values'][4]) - omitted / equity * 100) <= Decimal('0.0001')

    # Holdings list every held symbol and cash.
    assert len(published['portfolio.holdings']['table']['rows']) == 41

    # Decision log: top 11 weights + 其他; one disclosure per order, top 19 + 其他.
    [record] = published['portfolio.decision_log']['datasets'][0]['items']
    assert len(record['facts']) == 12
    by_weight = sorted(weights, key=lambda s: (-weights[s], s))
    assert [f['label'] for f in record['facts'][:11]] == by_weight[:11]
    assert record['facts'][11]['value'] == f"{Decimal(sum(weights[s] for s in by_weight[11:])) / 100:.2f}%"
    assert len(record['disclosures']) == 20 and record['disclosures'][-1]['id'] == 'other'
    assert '21' in record['disclosures'][-1]['label']


def test_every_recipe_slot_is_published_and_valid(rig):
    r = rig
    # Before any reconciliation, every unit still renders as a valid, explicitly pending cell.
    cells(r.status() | {'valuations': []})
    r.quote('US:AAA', '100'); r.quote('US:BBB', '50')
    r.watch('US:AAA', 'US:BBB')
    r.decide({'US:AAA': 3000, 'US:BBB': 2000}); r.execute(); r.publish(); r.fill()
    r.publish()
    state = r.fill(price='50')
    published = cells(state)
    assert {row['name'] for row in published['portfolio.holdings']['table']['rows']} >= {'US:AAA · 30 股', '现金'}
    assert published['portfolio.fill_log']['table']['rows']


def opening_account(r, cash, prices, shares):
    """A simulated account already holding `shares` of each priced symbol, acknowledged as opening."""
    r.configure(max_held=len(shares), opening_positions=json.dumps(
        [{'symbol': 'US:' + s.split('.')[0], 'shares': n} for s, n in shares.items()]))
    account = simulated(r, cash, prices)
    account.positions = dict(shares)
    return account


def test_weights_keep_cents_and_other_is_the_omitted_sum(rig):
    r = rig
    prices = {f'S{i:02d}.US': f'{200 + i}.37' for i in range(10)} | {'S10.US': '100.01', 'S11.US': '100.01'}
    opening_account(r, '899.49', prices, {s: 1 for s in prices})
    state = r.step()
    assert state['error'] is None
    slices = {s['id']: exact(s['value']) for s in cells(state)['portfolio.weights']['slices']}
    equity = Decimal(state['snapshot']['equity_usd'])
    assert sum(slices.values()) == equity == Decimal('3148.21')
    assert slices['other'] == Decimal('200.02') and slices['cash'] == Decimal('899.49')
    assert slices['US.S00'] == Decimal('200.37')


def test_weights_sum_to_equity_in_cents_with_sub_cent_values(rig):
    r = rig
    prices = {'AAA.US': '100.49', 'BBB.US': '0.3333', 'CCC.US': '0.6667'}
    opening_account(r, '899.49', prices, {'AAA.US': 1, 'BBB.US': 3, 'CCC.US': 1})
    state = r.step()
    slices = [exact(s['value']) for s in cells(state)['portfolio.weights']['slices']]
    assert sum(slices) == Decimal(state['snapshot']['equity_usd']).quantize(Decimal('0.01'))


def test_valuations_are_dated_by_trading_session(rig):
    r = rig
    friday = datetime(2026, 10, 2, 19, 55, tzinfo=timezone.utc)
    r.clock = lambda: friday + timedelta(minutes=1)
    r.quote('US:AAA', '100', at=friday)
    r.watch('US:AAA')
    for day in (3, 4):  # Saturday and Sunday: the newest quote is still Friday's
        r.clock = lambda day=day: datetime(2026, 10, day, 15, tzinfo=timezone.utc)
        r.step()
    monday = datetime(2026, 10, 5, 15, tzinfo=timezone.utc)
    r.quote('US:AAA', '101', at=monday)
    r.clock = lambda: monday + timedelta(minutes=1)
    state = r.step()
    assert [s['date'] for s in state['valuations']] == ['2026-10-02', '2026-10-05']
    previous = {i['id']: i for i in cells(state)['portfolio.nav']['items']}['previous']
    assert previous['label'] == '上一交易日估值 · 10.02'


def test_a_decision_that_traded_nothing_is_not_shown_as_success(rig):
    r = rig
    r.quote('US:AAA', '100'); r.watch('US:AAA')
    state = r.read(); state['response'] = {'status': 'not_submitted'}; r.write(state)
    r.decide({'US:AAA': 5000})
    state = r.execute()
    state = r.step()
    assert state['decisions'][0]['state'] == 'done'
    assert [o['state'] for o in state['decisions'][0]['orders']] == ['rejected']
    [record] = cells(state)['portfolio.decision_log']['datasets'][0]['items']
    badge = record['badges'][0]
    assert badge['tone'] != 'positive' and '未成交' in badge['value']


def test_weights_apportion_cents_across_entries(rig):
    """Three 0.334 holdings and no cash: per-entry rounding would publish 0.99 of a 1.00 equity."""
    r = rig
    prices = {'AAA.US': '0.334', 'BBB.US': '0.334', 'CCC.US': '0.334'}
    opening_account(r, '0', prices, {s: 1 for s in prices})
    state = r.step()
    assert state['error'] is None
    slices = [exact(s['value']) for s in cells(state)['portfolio.weights']['slices']]
    assert sum(slices) == Decimal('1.00') and sorted(slices) == [0, Decimal('0.33'), Decimal('0.33'), Decimal('0.34')]


def test_decision_amounts_use_every_fill(rig):
    """More fills than the fill log shows: amounts, ranking and 其他 still use the complete ledger."""
    r = rig
    prices = {'S00.US': 1} | {f'S{i:02d}.US': 10 + i for i in range(1, 21)}
    r.configure(max_held=21, max_watched=1)
    account = simulated(r, '10000', prices, split=True)
    r.watch(*('US:' + s.split('.')[0] for s in prices))
    r.decide({'US:S00': 600} | {f'US:S{i:02d}': 10 + i for i in range(1, 21)})
    r.request()
    for _ in range(25):
        state = r.step()
        if state['decisions'][0]['state'] == 'done':
            break
    assert state['decisions'][0]['state'] == 'done' and len(account.fills) == 620
    published = cells(state)
    assert len(published['portfolio.fill_log']['table']['rows']) == 500
    [record] = published['portfolio.decision_log']['datasets'][0]['items']
    first, *middle, other = record['disclosures']
    assert 'US:S00' in first['label'] and '已成交 600 股 · 成交金额 $600.00' in first['body']
    assert [d['label'].split(' ')[1] for d in middle] == [f'US:S{i:02d}' for i in range(20, 2, -1)]
    assert other['id'] == 'other' and other['body'] == '成交金额合计 $23.00'  # S01 + S02: 11 + 12


def test_sample_date_never_moves_backwards(rig):
    r = rig
    monday = datetime(2026, 10, 5, 15, tzinfo=timezone.utc)
    r.clock = lambda: monday + timedelta(minutes=1)
    r.quote('US:AAA', '100', at=monday)
    r.watch('US:AAA')
    # The newest quote is now older than the last sample (a halted or re-read symbol).
    r.quote('US:AAA', '99', at=datetime(2026, 10, 2, 19, 55, tzinfo=timezone.utc))
    state = r.step()
    assert [s['date'] for s in state['valuations']] == ['2026-10-05']
    assert state['snapshot']['date'] == '2026-10-05'
