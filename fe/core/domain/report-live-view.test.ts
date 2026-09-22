import { describe, expect, it } from 'vitest';

import { inlineTableBlockPayloadSchema, liveViewBlockPayloadSchema } from './report.js';
import { reportLiveViewSchema } from './report-live-view.js';

const overview = {
  version: 1, view: 'overview', updated: null,
  metrics: [{ label: 'Equity', value: '$100', detail: 'Account total', tone: 'neutral' }],
  notices: [], charts: [{ kind: 'meter', title: 'Capacity', unit: 'GB', detail: '', used: null, limit: null,
    usedLabel: 'Used', limitLabel: 'Capacity', emptyText: 'No observation', tone: 'neutral' }],
};

describe('native live-view contract', () => {
  it('requires a closed versioned reference with a plugin source', () => {
    const reference = { source: 'neige://plugin/operations/capacity', version: 1, view: 'overview' };
    expect(liveViewBlockPayloadSchema.parse(reference)).toEqual(reference);
    for (const invalid of [{ ...reference, source: 'https://example.com' }, { ...reference, version: 2 },
      { ...reference, view: 'script' }, { ...reference, src: '/app' }, { source: reference.source }]) {
      expect(liveViewBlockPayloadSchema.safeParse(invalid).success).toBe(false);
    }
  });

  it('enforces the total UTF-8 budget even when individual fields are valid', () => {
    const items = Array.from({ length: 50 }, (_, i) => ({ id: String(i), title: 'History',
      body: '😀'.repeat(8000), footer: '', sections: Array.from({ length: 4 }, () => ({ label: 'Observation', body: '😀'.repeat(8000) })) }));
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'cards', emptyText: '', items }).success).toBe(false);
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'cards', emptyText: '', items: items.slice(0, 2) }).success).toBe(true);
  });

  it('accepts an explicit unavailable budget without inventing zero', () => {
    expect(reportLiveViewSchema.parse(overview)).toEqual(overview);
    expect(inlineTableBlockPayloadSchema.safeParse(overview).success).toBe(false);
  });

  it.each([
    { ...overview, version: 2 }, { ...overview, view: 'html' }, { ...overview, script: 'alert(1)' },
    { ...overview, updated: { label: 'Measured', at: 'not-a-date' } },
    { ...overview, updated: { label: 'Measured', at: `2026-09-22T08:00:00.${'1'.repeat(200)}Z` } },
    { ...overview, charts: [{ ...overview.charts[0], used: 1, limit: 0 }] },
    { ...overview, charts: [{ kind: 'bars', title: 'B', unit: 'USD', emptyText: '', points: [{ label: 'X', value: Infinity, tone: 'negative' }] }] },
  ])('refuses unknown capabilities and malformed data %#', (value) => {
    expect(reportLiveViewSchema.safeParse(value).success).toBe(false);
  });

  it('keeps an over-limit amount exact for the renderer to disclose', () => {
    const value = { ...overview, charts: [{ ...overview.charts[0], used: 150, limit: 100 }] };
    expect(reportLiveViewSchema.parse(value)).toEqual(value);
  });

  it('reuses the existing strict table validator for collapsed details', () => {
    const table = { columns: [{ key: 'a', label: 'A' }], rows: [{ a: 'value' }] };
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'details', title: 'Orders', table }).success).toBe(true);
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'details', title: 'Orders', table: { ...table, rows: [{ b: 'extra' }] } }).success).toBe(false);
  });

  it('counts Unicode code points and bounds review text', () => {
    const view = { version: 1, view: 'cards', emptyText: '', items: [{ id: 'r', title: 'Review', body: '😀'.repeat(8000), sections: [], footer: '' }] };
    expect(reportLiveViewSchema.safeParse(view).success).toBe(true);
    expect(reportLiveViewSchema.safeParse({ ...view, items: [{ ...view.items[0], body: 'x'.repeat(8001) }] }).success).toBe(false);
  });

  it('refuses empty or duplicate item identities', () => {
    const item = { id: 'same', at: '2026-09-22T08:00:00Z', title: 'Event', detail: '', tone: 'neutral' };
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'activity', emptyText: '', items: [item, item] }).success).toBe(false);
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'activity', emptyText: '', items: [{ ...item, id: '' }] }).success).toBe(false);
    const card = { id: 'same', title: 'Review', body: '', sections: [], footer: '' };
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'cards', emptyText: '', items: [card, card] }).success).toBe(false);
  });
});
