import { describe, expect, it } from 'vitest';

import { inlineTableBlockPayloadSchema } from './report.js';
import { reportLiveViewSchema, signedBarLayout } from './report-live-view.js';

const overview = {
  version: 1, view: 'overview', asOf: null,
  metrics: [{ label: 'Equity', value: '$100', detail: 'Account total', tone: 'neutral' }],
  notices: [], charts: [{ kind: 'budget', title: 'Budget', unit: 'USD', detail: '', used: null, limit: null }],
};

describe('native live-view contract', () => {
  it('accepts an explicit unavailable budget without inventing zero', () => {
    expect(reportLiveViewSchema.parse(overview)).toEqual(overview);
    expect(inlineTableBlockPayloadSchema.safeParse(overview).success).toBe(false);
  });

  it.each([
    { ...overview, version: 2 }, { ...overview, view: 'html' }, { ...overview, script: 'alert(1)' },
    { ...overview, asOf: 'not-a-date' },
    { ...overview, asOf: `2026-09-22T08:00:00.${'1'.repeat(200)}Z` },
    { ...overview, charts: [{ kind: 'budget', title: 'B', unit: 'USD', detail: '', used: 1, limit: 0 }] },
    { ...overview, charts: [{ kind: 'bars', title: 'B', unit: 'USD', emptyText: '', points: [{ label: 'X', value: Infinity }] }] },
  ])('refuses unknown capabilities and malformed data %#', (value) => {
    expect(reportLiveViewSchema.safeParse(value).success).toBe(false);
  });

  it('keeps an over-limit amount exact for the renderer to disclose', () => {
    const value = { ...overview, charts: [{ kind: 'budget', title: 'B', unit: 'USD', detail: '', used: 150, limit: 100 }] };
    expect(reportLiveViewSchema.parse(value)).toEqual(value);
  });

  it('reuses the existing strict table validator for collapsed details', () => {
    const table = { columns: [{ key: 'a', label: 'A' }], rows: [{ a: 'value' }] };
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'details', title: 'Orders', table }).success).toBe(true);
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'details', title: 'Orders', table: { ...table, rows: [{ b: 'extra' }] } }).success).toBe(false);
  });

  it('counts Unicode code points and bounds review text', () => {
    const view = { version: 1, view: 'cards', emptyText: '', items: [{ id: 'r', title: 'Review', body: '😀'.repeat(8000), next: '', footer: '' }] };
    expect(reportLiveViewSchema.safeParse(view).success).toBe(true);
    expect(reportLiveViewSchema.safeParse({ ...view, items: [{ ...view.items[0], body: 'x'.repeat(8001) }] }).success).toBe(false);
  });

  it('refuses empty or duplicate item identities', () => {
    const item = { id: 'same', at: '2026-09-22T08:00:00Z', title: 'Event', detail: '', tone: 'neutral' };
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'activity', emptyText: '', items: [item, item] }).success).toBe(false);
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'activity', emptyText: '', items: [{ ...item, id: '' }] }).success).toBe(false);
    const card = { id: 'same', title: 'Review', body: '', next: '', footer: '' };
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'cards', emptyText: '', items: [card, card] }).success).toBe(false);
  });
});

it('draws positive, negative, and zero bars without fabricating a baseline result', () => {
  expect(signedBarLayout([0])).toEqual([{ start: 0, width: 0, zero: 0 }]);
  expect(signedBarLayout([50])).toEqual([{ start: 0, width: 100, zero: 0 }]);
  expect(signedBarLayout([-100, 50])).toEqual([
    { start: 0, width: 50, zero: 50 }, { start: 50, width: 25, zero: 50 },
  ]);
});
