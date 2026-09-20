import { describe, expect, it } from 'vitest';

import {
  minCompleteThrough, rebaseToFirst, resolvedSeriesSchema, seriesCurrencies, staleRevBodySchema,
  trackReportSeriesOperation,
} from './report-series.js';

/* The design's own sample, as the route serves it. */
const OK_ROW = {
  status: 'ok', as_of: '2026-09-11', resolved_at: '2026-09-12T08:00:00Z', pinned: false,
  view: 'normalized', field: 'close', period: 'day', range: '1Y',
  series: [
    {
      asset: 'US:NVDA', currency: 'USD', status: 'ok', complete_through: '2026-09-11',
      n: 251, first: ['2025-09-11', 118.2], last: ['2026-09-11', 176.9],
      change_pct: 49.66, high: 181.3, low: 101.4,
      points: [[1_757_548_800_000, 118.2], [1_789_084_800_000, 176.9]],
    },
    { asset: 'HK:9988', status: 'unknown_asset', reason: 'no such listing' },
  ],
};

describe('resolvedSeriesSchema', () => {
  it('reads the three statuses the kernel flattens on `status`', () => {
    const ok = resolvedSeriesSchema.parse(OK_ROW);
    expect(ok.status).toBe('ok');
    if (ok.status !== 'ok') throw new Error('unreachable');
    expect(ok.series[0]?.status).toBe('ok');
    expect(ok.series[1]).toEqual({ asset: 'HK:9988', status: 'unknown_asset', reason: 'no such listing' });

    const pending = resolvedSeriesSchema.parse({ status: 'pending', view: 'line', field: 'close', period: 'day', range: '1M' });
    expect(pending).toEqual({ status: 'pending', view: 'line', field: 'close', period: 'day', range: '1M' });
    const withReason = resolvedSeriesSchema.parse({
      status: 'pending', reason: 'plugin dev-neige-market is not running', view: 'line', field: 'close', period: 'day', range: '1M',
    });
    expect(withReason.status === 'pending' && withReason.reason).toBe('plugin dev-neige-market is not running');

    const unavailable = resolvedSeriesSchema.parse({
      status: 'unavailable', reason: 'timeout', resolved_at: '2026-09-12T08:00:00Z',
      view: 'bar', field: 'volume', period: 'week', range: '6M',
    });
    expect(unavailable.status).toBe('unavailable');
  });

  it('refuses an ok entry without its numbers rather than reading it as a failure', () => {
    // `status: ok` without `n`/`first`/`last` matches neither member: the
    // failed member refuses `ok`, so the row does not decode at all.
    const half = { ...OK_ROW, series: [{ asset: 'US:NVDA', status: 'ok' }] };
    expect(resolvedSeriesSchema.safeParse(half).success).toBe(false);
    expect(resolvedSeriesSchema.safeParse({ ...OK_ROW, view: 'pie' }).success).toBe(false);
    expect(resolvedSeriesSchema.safeParse({ ...OK_ROW, status: 'done' }).success).toBe(false);
  });

  it('reads the 409 body', () => {
    expect(staleRevBodySchema.parse({ current_rev: 4 })).toEqual({ current_rev: 4 });
    expect(staleRevBodySchema.safeParse({}).success).toBe(false);
  });
});

describe('trackReportSeriesOperation', () => {
  it('binds the request to the block and its rev, and names the detail', () => {
    const operation = trackReportSeriesOperation('w 1', 'b/1', 7, 'summary');
    expect(operation.method).toBe('GET');
    expect(operation.path).toBe('/api/tracks/w%201/report/series/b%2F1?rev=7&detail=summary');
    expect(trackReportSeriesOperation('w', 'b', 0, 'full').path).toBe('/api/tracks/w/report/series/b?rev=0&detail=full');
  });
});

describe('rebaseToFirst', () => {
  it('rebases every series to 100 at its first point', () => {
    const rebased = rebaseToFirst([[1, 50], [2, 75], [3, 25]]);
    expect(rebased.ok).toBe(true);
    if (!rebased.ok) throw new Error('unreachable');
    expect(rebased.points).toEqual([[1, 100], [2, 150], [3, 50]]);
    // The base is the FIRST point, never the last: a series that ended where
    // it started still starts at 100, and one that fell reads below it.
    const fell = rebaseToFirst([[1, 200], [2, 100]]);
    expect(fell.ok && fell.points[1]).toEqual([2, 50]);
  });

  it('uses the candle close column when asked', () => {
    const candles = rebaseToFirst([[1, 9, 12, 8, 10, 100], [2, 10, 13, 9, 12, 100]], 4);
    expect(candles.ok && candles.points).toEqual([[1, 100], [2, 120]]);
  });

  it('refuses a first value that is not positive', () => {
    expect(rebaseToFirst([[1, 0], [2, 5]])).toEqual({ ok: false, reason: 'first value ≤ 0' });
    expect(rebaseToFirst([[1, -3], [2, 5]])).toEqual({ ok: false, reason: 'first value ≤ 0' });
    expect(rebaseToFirst([])).toEqual({ ok: false, reason: 'first value ≤ 0' });
  });
});

describe('summary helpers', () => {
  it('reports the earliest complete_through and the distinct currencies of the ok entries', () => {
    const series = [
      { asset: 'US:NVDA', currency: 'USD', status: 'ok' as const, complete_through: '2026-09-11', n: 2, first: ['a', 1] as [string, number], last: ['b', 2] as [string, number], change_pct: 100, high: 2, low: 1 },
      { asset: 'HK:9988', currency: 'HKD', status: 'ok' as const, complete_through: '2026-09-09', n: 2, first: ['a', 1] as [string, number], last: ['b', 2] as [string, number], change_pct: 100, high: 2, low: 1 },
      { asset: 'US:AAPL', currency: 'USD', status: 'ok' as const, complete_through: '2026-09-10', n: 2, first: ['a', 1] as [string, number], last: ['b', 2] as [string, number], change_pct: 100, high: 2, low: 1 },
      { asset: 'SH:600519', status: 'unavailable', reason: 'no data in range' },
    ];
    expect(minCompleteThrough(series)).toBe('2026-09-09');
    expect(seriesCurrencies(series)).toEqual(['USD', 'HKD']);
    expect(minCompleteThrough(series.slice(3))).toBeNull();
    expect(seriesCurrencies([])).toEqual([]);
  });
});
