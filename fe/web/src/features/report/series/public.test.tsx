// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { ChartSeriesPayload } from '../../../../../core/domain/report.ts';
import type { OkResolvedSeries, SeriesEntry, SeriesResolution } from '../../../../../core/domain/report-series.ts';
import { ReportSeriesBlock } from './public.tsx';

afterEach(cleanup);

const DAY = 86_400_000;
const SOURCE = 'neige://plugin/dev-neige-market/market.series';

function payload(overrides: Partial<ChartSeriesPayload> = {}): ChartSeriesPayload {
  return { source: SOURCE, series: ['US:NVDA', 'HK:9988'], range: '1Y', ...overrides };
}

function okEntry(asset: string, values: readonly number[], overrides: Partial<Extract<SeriesEntry, { status: 'ok' }>> = {}): SeriesEntry {
  const first = values[0] ?? 0;
  const last = values[values.length - 1] ?? 0;
  return {
    asset, currency: asset.startsWith('HK') ? 'HKD' : 'USD', status: 'ok', complete_through: '2026-09-11',
    n: values.length, first: ['2026-09-01', first], last: ['2026-09-10', last],
    change_pct: first === 0 ? null : Math.round(((last - first) / first) * 10_000) / 100,
    high: Math.max(...values), low: Math.min(...values),
    points: values.map((value, index) => [index * DAY, value]),
    ...overrides,
  };
}

function ok(series: readonly SeriesEntry[], overrides: Partial<OkResolvedSeries> = {}): OkResolvedSeries {
  return {
    status: 'ok', as_of: '2026-09-11', resolved_at: '2026-09-12T08:00:00Z', pinned: true,
    view: 'line', field: 'close', period: 'day', range: '1Y', series: [...series], ...overrides,
  };
}

function draw(resolution: SeriesResolution | undefined, overrides: Partial<ChartSeriesPayload> = {}) {
  return render(<ReportSeriesBlock payload={payload(overrides)} blockId="b-1" rev={2}
    resolve={() => resolution} />);
}

function polylinePoints(container: HTMLElement): number[][][] {
  return [...container.querySelectorAll('polyline[data-nc-series]')].map((node) =>
    (node.getAttribute('points') ?? '').split(' ').map((pair) => pair.split(',').map(Number)));
}

describe('ReportSeriesBlock', () => {
  it('draws one polyline per ok series, none for a failed one', () => {
    const { container } = draw(ok([
      okEntry('US:NVDA', [118.2, 125, 101.4, 176.9]),
      okEntry('HK:9988', [100, 90]),
      { asset: 'SH:600519', status: 'unknown_asset', reason: 'no such listing' },
    ]));
    const lines = container.querySelectorAll('polyline[data-nc-series]');
    expect(lines.length).toBe(2);
    expect([...lines].map((line) => line.getAttribute('data-nc-series'))).toEqual(['US:NVDA', 'HK:9988']);
    expect(container.querySelector('svg')?.getAttribute('aria-label')).toContain('2 series');
    // The legend carries the summary's own numbers, and the failed asset's verdict.
    expect(container.textContent).toContain('+49.66%');
    expect(container.textContent).toContain('unknown_asset — no such listing');
    expect(container.textContent).toContain('USD / HKD');
    expect(container.textContent).toContain('as of 2026-09-11');
  });

  it('rebases every series to 100 at its first point', () => {
    const { container } = draw(ok([
      okEntry('US:NVDA', [50, 75, 100]),
      okEntry('HK:9988', [200, 100]),
    ], { view: 'normalized' }));
    const [nvda, baba] = polylinePoints(container);
    // Both first points sit on the same y: 100 is the same height whatever
    // the price. Dividing by the LAST value instead would put the last points
    // level and the first ones apart.
    expect(nvda?.[0]?.[1]).toBeCloseTo(baba?.[0]?.[1] ?? Number.NaN, 6);
    expect(nvda?.[nvda.length - 1]?.[1]).not.toBeCloseTo(baba?.[baba.length - 1]?.[1] ?? Number.NaN, 6);
    // …and the legend says where each ended, from 100: doubled, and halved.
    const legend = screen.getByRole('list', { name: 'Series' }).textContent ?? '';
    expect(legend).toContain('US:NVDA');
    expect(legend).toContain('100 → 200.00');
    expect(legend).toContain('100 → 50.00');
    expect(container.querySelector('svg')?.getAttribute('aria-label')).toContain('rebased to 100');
  });

  it('marks a series whose first value is not positive as not normalizable and draws the rest', () => {
    const { container } = draw(ok([
      okEntry('US:NVDA', [0, 5, 9]),
      okEntry('HK:9988', [10, 20]),
    ], { view: 'normalized' }));
    expect(container.querySelectorAll('polyline[data-nc-series]').length).toBe(1);
    expect(container.querySelector('polyline[data-nc-series]')?.getAttribute('data-nc-series')).toBe('HK:9988');
    expect(container.textContent).toContain('cannot normalize (first value ≤ 0)');
  });

  it('draws bars per series and per period, from zero', () => {
    const { container } = draw(ok([
      okEntry('US:NVDA', [3, 1, 2]),
      okEntry('HK:9988', [1, 2, 3]),
    ], { view: 'bar', field: 'volume' }));
    const bars = [...container.querySelectorAll('rect[data-nc-series]')];
    expect(bars.length).toBe(6);
    // The smallest value still has height: bars grow from zero, not from the minimum.
    expect(bars.every((bar) => Number(bar.getAttribute('height')) > 1)).toBe(true);
  });

  it('draws candles through the shared candles figure', () => {
    const { container } = draw(ok([
      okEntry('US:NVDA', [1, 2], { points: [[0, 100, 106, 99, 105, 1000], [DAY, 105, 107, 100, 101, 1200]] }),
    ], { view: 'candles' }), { series: ['US:NVDA'], view: 'candles', overlays: ['ma20'] });
    expect(container.querySelectorAll('svg rect').length).toBeGreaterThanOrEqual(2);
    expect(container.querySelector('svg')?.getAttribute('aria-label')).toContain('US:NVDA: 2 candles');
    expect(container.textContent).toContain('涨（空心）');
  });

  it('applies ma overlays to the line view as dashed lines in the series hue', () => {
    const values = Array.from({ length: 25 }, (_, index) => 100 + index);
    const { container } = draw(ok([okEntry('US:NVDA', values)]), { series: ['US:NVDA'], overlays: ['ma20'] });
    const overlays = container.querySelectorAll('polyline:not([data-nc-series])');
    expect(overlays.length).toBe(1);
    // A 20-day average has 6 values over 25 points.
    expect((overlays[0]?.getAttribute('points') ?? '').split(' ').length).toBe(6);
    expect(container.textContent).toContain('MA20');
  });

  it('shows the pending reason and the unavailable reason', () => {
    draw({ status: 'pending', reason: 'plugin dev-neige-market is not running', view: 'line', field: 'close', period: 'day', range: '1Y' });
    expect(screen.getByRole('note').textContent).toBe('Pending — plugin dev-neige-market is not running');
    cleanup();
    draw({ status: 'pending', view: 'line', field: 'close', period: 'day', range: '1Y' });
    expect(screen.getByRole('note').textContent).toContain('Pending — the kernel is fetching');
    cleanup();
    draw({ status: 'unavailable', reason: 'timeout after 30s', resolved_at: '2026-09-12T08:00:00Z', view: 'line', field: 'close', period: 'day', range: '1Y' });
    expect(screen.getByRole('note').textContent).toBe('Unavailable — timeout after 30s');
    cleanup();
    draw({ status: 'loading' }, { caption: 'Big tech' });
    expect(screen.getByRole('note').textContent).toBe('Loading …');
    expect(screen.getByText('Big tech')).toBeTruthy();
    cleanup();
    draw(undefined);
    expect(screen.getByRole('note').textContent).toBe('Loading …');
    cleanup();
    draw({ status: 'error', message: 'Internal Server Error' });
    expect(screen.getByRole('note').textContent).toContain('Could not load this chart: Internal Server Error');
  });

  it('stale rev 409 is a wait, not an error', () => {
    const { container } = draw({ status: 'stale-rev', current_rev: 3 });
    const note = screen.getByRole('note');
    expect(note.textContent).toBe('Waiting for the report to refresh.');
    expect(container.textContent).not.toMatch(/error|could not|fail/i);
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });

  // The status line (#1693): three facts, three sentences, and never the
  // phrase "not pinned" — a frozen block whose last day has not arrived was
  // read as "the author did not freeze this".
  it('frozen and pinned: says frozen-at and pinned, once', () => {
    const { container } = draw(ok([okEntry('US:NVDA', [1, 2])], { pinned: true, as_of: '2026-09-11' }), { as_of: '2026-09-11' });
    expect(container.textContent).toContain('frozen at 2026-09-11 · pinned');
    expect(container.textContent).not.toContain('pin pending');
    expect(container.textContent).not.toContain('not pinned');
    // The sentence carries `as_of`; the separate "as of" span would say it twice.
    expect(container.textContent).not.toContain('as of 2026-09-11');
  });

  it('frozen with an incomplete last day: says frozen-at and complete-through, never "not pinned"', () => {
    const { container } = draw(ok([
      okEntry('US:NVDA', [1, 2], { complete_through: '2026-09-11' }),
      okEntry('HK:9988', [1, 2], { complete_through: '2026-09-09' }),
    ], { pinned: false, as_of: '2026-09-12' }), { as_of: '2026-09-12' });
    expect(container.textContent).toContain('frozen at 2026-09-12 · complete through 2026-09-09 · pin pending');
    expect(container.textContent).not.toContain('not pinned');
    expect(container.textContent).not.toContain('as of 2026-09-12');
  });

  it('live: says complete-through and resolved-at, and keeps the kernel\'s as_of', () => {
    const { container } = draw(ok([okEntry('US:NVDA', [1, 2])], { pinned: false }));
    expect(container.textContent).toContain('live · complete through 2026-09-11 · resolved 2026-09-12T08:00:00Z');
    expect(container.textContent).toContain('as of 2026-09-11');
    expect(container.textContent).not.toContain('frozen');
    expect(container.textContent).not.toContain('pinned');
  });

  it('frozen with no resolved asset: complete-through is unknown', () => {
    const { container } = draw(ok([
      { asset: 'US:NVDA', status: 'unknown_asset', reason: 'no such listing' },
    ], { pinned: false, as_of: '2026-09-12' }), { as_of: '2026-09-12', series: ['US:NVDA'] });
    expect(container.textContent).toContain('frozen at 2026-09-12 · complete through unknown · pin pending');
    expect(container.textContent).not.toContain('not pinned');
  });

  it('says so when the surface carries no resolver', () => {
    render(<ReportSeriesBlock payload={payload({ caption: 'Big tech' })} blockId="b-1" rev={1} />);
    expect(screen.getByRole('note').textContent).toContain('this view does not carry its data');
    expect(screen.getByText('Big tech')).toBeTruthy();
  });

  it('paints from tokens, never from literal colours', () => {
    const { container } = draw(ok([
      okEntry('US:NVDA', [1, 2, 3]), okEntry('HK:9988', [1, 2, 3]), okEntry('SH:600519', [1, 2, 3]),
      okEntry('SZ:000001', [1, 2, 3]), okEntry('US:AAPL', [1, 2, 3]), okEntry('US:MSFT', [1, 2, 3]),
      okEntry('US:AMZN', [1, 2, 3]), okEntry('US:META', [1, 2, 3]),
    ]), { overlays: ['ma20', 'ma60'] });
    const markup = container.innerHTML;
    expect(markup).not.toMatch(/#[0-9a-f]{3,6}\b/i);
    expect(markup).not.toMatch(/rgb\(/i);
    expect(markup).not.toMatch(/style="/);
    expect(container.querySelectorAll('polyline[data-nc-series]').length).toBe(8);
  });
});
