import { describe, expect, it } from 'vitest';

import { isCalendarDate, readTrackReport, TRACK_REPORT_CARD_KIND } from './report.js';
import type { CardWire } from './track.js';

/*
 * `chart.series` (#1628 S1) through the same entrance the renderer uses:
 * `readTrackReport` over a track-report card. Every case here is a wire block
 * the kernel would have accepted or refused for the same reason, so a payload
 * the kernel stores is exactly one this reads, and a payload the kernel
 * refuses degrades to `unsupported` here rather than reaching a renderer.
 */

const SOURCE = 'neige://plugin/dev-neige-market/market.series';

function card(payload: unknown): CardWire {
  return {
    id: 'c1', track_id: 'w1', kind: TRACK_REPORT_CARD_KIND, title: null, sort: 0,
    payload, deletable: false, created_at: 0, updated_at: 0,
  };
}

function readSeries(payload: Record<string, unknown>) {
  const report = readTrackReport([card({
    body: 'x', blocks: [{ id: 'b-1', kind: 'chart.series', rev: 1, payload }],
  })]);
  return report?.blocks?.[0];
}

describe('readTrackReport — chart.series', () => {
  it('reads a legal block as chart.series with its payload', () => {
    expect(readSeries({ source: SOURCE, series: ['US:NVDA', 'HK:9988'], as_of: '2026-09-10' })).toEqual({
      id: 'b-1',
      kind: 'chart.series',
      rev: 1,
      payload: { source: SOURCE, series: ['US:NVDA', 'HK:9988'], as_of: '2026-09-10' },
    });
  });

  // #1628 S4 — the data request is bound to the block revision, so a series
  // block is the one kind that carries `rev` out of the wire. Without one it
  // cannot ask for anything and degrades like an unreadable payload.
  it('carries the wire rev on a series block and degrades one without it', () => {
    const report = readTrackReport([card({
      body: 'x',
      blocks: [
        { id: 'b-1', kind: 'chart.series', rev: 7, payload: { source: SOURCE, series: ['US:NVDA'] } },
        { id: 'b-2', kind: 'chart.series', payload: { source: SOURCE, series: ['US:NVDA'] } },
        { id: 'b-3', kind: 'prose', payload: { markdown: 'no rev needed' } },
      ],
    })]);
    expect(report?.blocks?.map((block) => (block.kind === 'chart.series' ? block.rev : block.kind)))
      .toEqual([7, 'unsupported', 'prose']);
  });

  it('accepts every optional field at once, and candles with one series', () => {
    expect(readSeries({
      source: SOURCE, series: ['US:NVDA'], field: 'volume', range: '3M', period: 'month',
      view: 'bar', as_of: '2024-02-29', caption: 'Volume',
    })?.kind).toBe('chart.series');
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], view: 'candles', overlays: ['ma20'] })?.kind)
      .toBe('chart.series');
  });

  // The calendar, not a `Date`: `new Date('2026-02-30')` rolls over to March
  // and would accept exactly the payload the kernel refuses.
  it('degrades a cutoff that is not a calendar day to unsupported', () => {
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], as_of: '2026-02-30' }))
      .toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: 'chart.series' });
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], as_of: '2027-02-29' })?.kind).toBe('unsupported');
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], as_of: '2026/09/10' })?.kind).toBe('unsupported');
  });

  it('accepts a cutoff in the future — no clock, no upper bound', () => {
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], as_of: '2099-01-01' })?.kind).toBe('chart.series');
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], as_of: '2028-02-29' })?.kind).toBe('chart.series');
  });

  it('degrades an asset without a venue to unsupported', () => {
    expect(readSeries({ source: SOURCE, series: ['NVDA'] }))
      .toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: 'chart.series' });
    expect(readSeries({ source: SOURCE, series: ['us:NVDA'] })?.kind).toBe('unsupported');
  });

  it('degrades range 1M with period month to unsupported', () => {
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], range: '1M', period: 'month' }))
      .toEqual({ id: 'b-1', kind: 'unsupported', declaredKind: 'chart.series' });
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], range: '1M', period: 'week' })?.kind)
      .toBe('chart.series');
    expect(readSeries({ source: SOURCE, series: ['US:NVDA'], range: '3M', period: 'month' })?.kind)
      .toBe('chart.series');
  });

  it.each([
    ['a missing source', { series: ['US:NVDA'] }],
    ['a missing series', { source: SOURCE }],
    ['an empty series', { source: SOURCE, series: [] }],
    ['nine series', { source: SOURCE, series: Array.from({ length: 9 }, (_, i) => `US:A${i}`) }],
    ['a repeated asset', { source: SOURCE, series: ['US:NVDA', 'US:NVDA'] }],
    ['an unknown field', { source: SOURCE, series: ['US:NVDA'], points: [] }],
    ['a source of the wrong shape', { source: 'neige://plugin/one-segment', series: ['US:NVDA'] }],
    ['candles with two series', { source: SOURCE, series: ['US:NVDA', 'HK:9988'], view: 'candles' }],
    ['candles with a field', { source: SOURCE, series: ['US:NVDA'], view: 'candles', field: 'close' }],
    ['overlays under a bar view', { source: SOURCE, series: ['US:NVDA'], view: 'bar', overlays: ['ma20'] }],
    ['an unknown overlay', { source: SOURCE, series: ['US:NVDA'], overlays: ['ma50'] }],
    ['an unknown view', { source: SOURCE, series: ['US:NVDA'], view: 'area' }],
  ])('degrades %s to unsupported', (_label, payload) => {
    expect(readSeries(payload)?.kind).toBe('unsupported');
  });
});

describe('isCalendarDate', () => {
  it('follows the Gregorian leap-year rule', () => {
    expect(isCalendarDate('1900-02-29')).toBe(false);
    expect(isCalendarDate('2000-02-29')).toBe(true);
    expect(isCalendarDate('2024-02-29')).toBe(true);
    expect(isCalendarDate('2100-02-29')).toBe(false);
    expect(isCalendarDate('2026-04-31')).toBe(false);
    expect(isCalendarDate('2026-13-01')).toBe(false);
    expect(isCalendarDate('2026-09-00')).toBe(false);
    expect(isCalendarDate('2026-9-10')).toBe(false);
    expect(isCalendarDate('2026-09-10')).toBe(true);
  });
});
