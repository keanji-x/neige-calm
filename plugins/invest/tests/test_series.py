"""`series_show`: the `market.series` contract for US symbols, through the production App, its SDK
subprocess runner and the prescribed SDK transport (no network)."""
from datetime import date, datetime, timedelta, timezone
import json

import pytest

from host import Host
from invest import series
from invest.broker import BrokerError
from rig import PLANNER, ROOT, WORKER

REPO = ROOT.parents[1]
SEAM = json.loads((REPO / 'crates/calm-server/tests/fixtures/market_series_reply.json').read_text())
NOW_MS = 1_789_387_200_000  # 2026-09-14T12:00:00Z
DEADLINE = NOW_MS + 30_000


def day(ts_ms):
    return (date(1970, 1, 1) + timedelta(days=ts_ms // 86_400_000)).isoformat()


def ms(raw):
    return (date.fromisoformat(raw) - date(1970, 1, 1)).days * 86_400_000


def row(when, close, *, open_='1', high=None, low='0.5', volume=100):
    return [when, open_, str(high if high is not None else max(float(close), 1.0)), low, str(close), str(volume)]


def prescribe(rig, **symbols):
    """`symbols`: SDK symbol (`NVDA_US` for `NVDA.US`) → `(complete_through, [row, …])`."""
    state = rig.read()
    state['series'] = {k.replace('_', '.'): {'complete_through': probe, 'bars': bars}
                       for k, (probe, bars) in symbols.items()}
    rig.write(state)


def request(**changes):
    return {'series': ['US:NVDA'], 'fields': ['close'], 'period': 'day', 'mode': 'frozen',
            'start': '2026-08-10', 'as_of': '2026-09-10', 'deadline_ms': DEADLINE, **changes}


def show(rig, args, now_ms=NOW_MS):
    return series.show(rig.broker, args, now_ms)


def series_calls(rig):
    return [c['request'] for c in rig.calls() if c['method'] == 'series']


def without_descriptions(node):
    if isinstance(node, dict):
        return {k: without_descriptions(v) for k, v in node.items() if k != 'description'}
    if isinstance(node, list):
        return [without_descriptions(v) for v in node]
    return node


def test_series_contract_matches_market_series(rig):
    # The input contract is market.series's, key for key; only the prose differs (US only).
    market = json.loads((REPO / 'plugins/market/manifest.json').read_text())
    [theirs] = [t for t in market['exposes_tools'] if t['name'] == 'market.series']
    [ours] = [t for t in json.loads((ROOT / 'manifest.json').read_text())['exposes_tools']
              if t['name'] == series.TOOL]
    assert without_descriptions(ours['input_schema']) == without_descriptions(theirs['input_schema'])
    assert ours['annotations']['readOnlyHint'] is True and ours['annotations']['openWorldHint'] is True
    assert list(ours['input_schema']['required']) == list(series.KEYS)

    # The kernel seam (#1628): the seam's request answers the seam's reply for its US asset, from
    # daily bars on exactly those dates; the HK asset is outside the US-only contract.
    [nvda] = [s for s in SEAM['reply']['series'] if s['asset'] == 'US:NVDA']
    prescribe(rig, NVDA_US=(nvda['complete_through'],
                            [row('2026-08-03', '90')] + [row(day(t), v) for t, v in nvda['points']]))
    reply = show(rig, SEAM['request'] | {'deadline_ms': DEADLINE})
    assert 'isError' not in reply, reply
    entries = reply['structuredContent']['series']
    assert entries[0] == nvda
    assert entries[1]['asset'] == 'HK:9988' and entries[1]['status'] == 'unknown_asset' and 'US' in entries[1]['reason']
    assert set(entries[1]) == {'asset', 'status', 'reason'}
    # One SDK request for every US asset: the SDK symbol over the window with its 14-day margin.
    assert series_calls(rig) == [{'symbols': ['NVDA.US'], 'start': '2026-07-27', 'end': '2026-09-10'}]
    assert reply['content'][0]['text'].startswith('US:NVDA: ok, 6 points through 2026-09-11 (USD); HK:9988: ')


def test_series_refuses_agent_caller(rig):
    prescribe(rig, NVDA_US=('2026-09-11', [row('2026-08-03', '1'), row('2026-09-09', '2'), row('2026-09-10', '3')]))
    host = Host(rig.home, rig.root, rig.values)
    try:
        for caller in (PLANNER, WORKER, {}):
            frame = host.response('tools/call', {'name': series.TOOL, 'arguments': request(), '_meta': {
                'dev.neige/track': {'id': 'owner'}, 'dev.neige/caller': caller}})
            assert frame['error']['code'] == -32403 and 'never an agent' in frame['error']['message'], frame
        frame = host.response('tools/call', {'name': series.TOOL, 'arguments': request(), '_meta': {}})
        assert frame['error']['code'] == -32403, frame
        assert series_calls(rig) == [], 'a refused call never reaches the SDK'
        # The chart resolver's shape: the host's Track and no caller.
        reply = host.tool(series.TOOL, request(deadline_ms=int(datetime.now(timezone.utc).timestamp() * 1000) + 30_000),
                          track='owner')
        assert reply['structuredContent']['series'][0]['status'] == 'ok', reply
        assert len(series_calls(rig)) == 1
    finally:
        host.close()


def test_series_requires_and_checks_every_key(rig):
    for key in series.KEYS:
        args = request()
        del args[key]
        reply = show(rig, args)
        assert reply['isError'] and f'`{key}`' in reply['content'][0]['text'], (key, reply)
    bad = [('series', []), ('series', [f'US:A{i}' for i in range(9)]), ('series', [1]), ('series', 'US:NVDA'),
           ('fields', []), ('fields', ['adj_close']), ('period', 'hour'), ('mode', 'relaxed'),
           ('start', '2026-02-30'), ('start', '20260810'), ('as_of', '2026/09/10'), ('deadline_ms', 1.5),
           ('deadline_ms', 'soon'), ('deadline_ms', True)]
    for key, value in bad:
        reply = show(rig, request(**{key: value}))
        assert reply['isError'] and f'`{key}`' in reply['content'][0]['text'], (key, value, reply)
    assert show(rig, request(start='2026-09-11'))['isError']
    assert show(rig, request(extra=1))['isError']
    assert series_calls(rig) == [], 'a malformed request never reaches the SDK'


def test_series_deadline_refuses_before_any_sdk_call(rig):
    reply = show(rig, request(deadline_ms=NOW_MS - 1))
    assert reply['isError'] and 'deadline exceeded' in reply['content'][0]['text']
    assert series_calls(rig) == []


@pytest.mark.parametrize('mode', ['live', 'frozen'])
def test_series_us_bar_needs_a_later_daily_bar(rig, mode):
    bars = [row('2026-08-03', '1'), row('2026-09-08', '2'), row('2026-09-09', '3'), row('2026-09-10', '4')]
    # The newest listed bar is the cutoff day itself: nothing proves it closed, so it is left out
    # even in live daily mode (US is never relaxed).
    prescribe(rig, NVDA_US=('2026-09-10', bars))
    [entry] = show(rig, request(mode=mode))['structuredContent']['series']
    assert entry['complete_through'] == '2026-09-10'
    assert [p[0] for p in entry['points']] == [ms('2026-09-08'), ms('2026-09-09')]
    prescribe(rig, NVDA_US=('2026-09-11', bars))
    [entry] = show(rig, request(mode=mode))['structuredContent']['series']
    assert [p[0] for p in entry['points']] == [ms('2026-09-08'), ms('2026-09-09'), ms('2026-09-10')]


def test_series_aggregates_weeks_and_months(rig):
    bars = [row('2026-07-31', '7'),
            row('2026-08-03', '10', open_='10', high='12', low='9', volume=100),   # Monday
            row('2026-08-04', '14', open_='11', high='15', low='10', volume=200),
            row('2026-08-07', '9', open_='14', high='14.5', low='8', volume=50),   # Friday
            row('2026-08-10', '9.2', open_='9', high='9.5', low='8.5', volume=10),
            row('2026-09-01', '20', open_='20', high='21', low='19', volume=5),
            row('2026-09-30', '22', open_='21', high='23', low='18', volume=7)]
    prescribe(rig, NVDA_US=('2026-10-01', bars))
    fields = ['open', 'high', 'low', 'close', 'volume']
    weeks = show(rig, request(period='week', start='2026-08-03', as_of='2026-09-30', fields=fields))
    [entry] = weeks['structuredContent']['series']
    assert entry['points'][:2] == [[ms('2026-08-03'), 10.0, 15.0, 8.0, 9.0, 350.0],
                                   [ms('2026-08-10'), 9.0, 9.5, 8.5, 9.2, 10.0]]
    # The week of 09-28 ends 10-04, after the cutoff: never included.
    assert entry['points'][-1][0] == ms('2026-08-31')
    months = show(rig, request(period='month', start='2026-08-01', as_of='2026-09-30', fields=fields))
    [entry] = months['structuredContent']['series']
    assert entry['points'] == [[ms('2026-08-01'), 10.0, 15.0, 8.0, 9.2, 360.0],
                               [ms('2026-09-01'), 20.0, 23.0, 18.0, 22.0, 12.0]]


def test_series_depth_and_near_end_checks(rig):
    cases = [([row('2026-08-30', '1'), row('2026-09-09', '2'), row('2026-09-10', '3')], 'lookback exceeds source depth'),
             ([row('2026-08-03', '1'), row('2026-08-11', '2'), row('2026-08-12', '3')], 'no data near cutoff'),
             ([row('2026-08-03', '1'), row('2026-09-10', '2')], 'no data in range'),
             ([row('2026-08-03', 'NaN'), row('2026-09-09', '2'), row('2026-09-10', '3')], 'non-numeric')]
    for bars, reason in cases:
        prescribe(rig, NVDA_US=('2026-09-11', bars))
        [entry] = show(rig, request())['structuredContent']['series']
        assert entry['status'] == 'unavailable' and reason in entry['reason'], (reason, entry)


def test_series_one_assets_failure_never_fails_another(rig):
    prescribe(rig, NVDA_US=('2026-09-11', [row('2026-08-03', '1'), row('2026-09-09', '2'), row('2026-09-10', '3')]))
    reply = show(rig, request(series=['US:NVDA', 'US:NONE', 'CRYPTO:BTC', 'BTC']))
    statuses = [(e['asset'], e['status']) for e in reply['structuredContent']['series']]
    assert statuses == [('US:NVDA', 'ok'), ('US:NONE', 'unavailable'), ('CRYPTO:BTC', 'unknown_asset'),
                        ('BTC', 'unknown_asset')]
    assert series_calls(rig)[-1]['symbols'] == ['NONE.US', 'NVDA.US']

    class Down:
        def series(self, *args):
            raise BrokerError('Broker SDK process failed; outcome may be unknown')
    reply = series.show(Down(), request(series=['US:NVDA', 'HK:700']), NOW_MS)
    statuses = [(e['asset'], e['status']) for e in reply['structuredContent']['series']]
    assert statuses == [('US:NVDA', 'unavailable'), ('HK:700', 'unknown_asset')]
