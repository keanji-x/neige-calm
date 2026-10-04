"""Portfolio data units: valid against the kernel's contract, and aggregated without losing value."""
from datetime import timedelta
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


def valid(unit):
    jsonschema.Draft202012Validator(UNIT_SCHEMA).validate(unit)
    assert len(json.dumps(unit, ensure_ascii=False, separators=(',', ':')).encode()) <= MAX_UNIT_BYTES
    return unit


def cells(state):
    published = units(state)
    assert set(published) == unit_kinds()
    return {kind: valid(unit)['cell'] for kind, unit in published.items()}


def simulated(r, cash, prices):
    account = SimulatedAccount(cash, prices, lambda: r.clock())
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
            assert abs(sum(point['values']) - 100) < 1e-3
            assert abs(point['values'][4] - float(omitted / equity * 100)) < 1e-3

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
