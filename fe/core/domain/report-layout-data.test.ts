import { describe, expect, it } from 'vitest';

import { layoutCell, layoutChartData, layoutShareTotal, resolveLayoutData } from './report-layout-data.js';
import { reportLayoutSchema, type LayoutChart, type LayoutTable } from './report-layout.js';

function chart(overrides: Partial<LayoutChart> = {}): LayoutChart {
  return { kind: 'chart', title: 'History', span: 1, chart: 'line', data: { rows: [] },
    x: 'at', y: 'value', color: '#123456', height: 260, ...overrides };
}
function table(): LayoutTable {
  return { kind: 'table', title: 'Rows', span: 1, data: { source: 'neige://plugin/market/holdings' },
    exclude: { key: 'asset', value: 'Total' }, total: { row: { key: 'asset', value: 'Total' }, key: 'value' },
    columns: [{ key: 'value', label: 'Weight', format: 'share', digits: 1, fallbackKey: 'price' }] };
}
function parse(item: unknown) {
  return reportLayoutSchema.safeParse({ version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [item] });
}

describe('layout contract', () => {
  it('preserves own scalar row keys without interpreting prototype names', () => {
    const input = JSON.parse('{"__proto__":"own value","constructor":"own constructor","value":1}') as Record<string, string | number>;
    const result = parse(chart({ data: { rows: [input] } }));
    expect(result.success).toBe(true);
    if (!result.success) throw new Error('Expected valid row');
    const data = result.data.items[0].data;
    if (!('rows' in data)) throw new Error('Expected inline data');
    expect(Object.hasOwn(data.rows[0], '__proto__')).toBe(true);
    expect(data.rows[0]['__proto__']).toBe('own value');
    expect(Object.getPrototypeOf(data.rows[0])).toBe(Object.prototype);
  });
  it('rejects malformed, executable, ambiguous and incomplete configuration', () => {
    expect(parse(chart()).success).toBe(true);
    for (const item of [chart({ span: 2 }), { ...chart(), html: '<script/>' }, chart({ color: '#123456\n' }),
      chart({ data: { source: 'neige://plugin/market/history\n' } }), chart({ ranges: [30, 30], defaultRange: 30 }),
      chart({ ranges: [30] }), chart({ defaultRange: 30 }), chart({ chart: 'donut', ranges: [30], defaultRange: 30 }),
      chart({ labelSuffixKey: 'venue' }), chart({ chart: 'donut', labelSuffixKey: '' }),
      chart({ data: { source: 'neige://plugin/market/history', annotations: { keys: ['asset'], rows: [{ asset: 'A' }, { asset: 'A' }] } } }),
      chart({ data: { source: 'neige://plugin/market/history', annotations: { keys: ['asset'], rows: [{}] } } }),
      { ...table(), total: undefined },
    ]) expect(parse(item).success).toBe(false);
  });
});

describe('source-backed chart observations', () => {
  it('distinguishes identical asset codes with a template-selected label suffix', () => {
    const item = { ...chart({ chart: 'donut', x: 'asset', data: { rows: [
      { asset: 'W', venue: 'US', value: 10 }, { asset: 'W', venue: 'CRYPTO', value: 20 },
    ] } }), labelSuffixKey: 'venue' };
    expect(layoutChartData(item, resolveLayoutData(item))).toMatchObject({ kind: 'ready', points: [
      { x: 'W · US', value: 10 }, { x: 'W · CRYPTO', value: 20 },
    ] });
    const missing = { ...item, data: { rows: [{ asset: 'W', value: 10 }] } };
    expect(layoutChartData(missing, resolveLayoutData(missing)).kind).toBe('unavailable');
  });
  it('accepts absent live-table captions just like the native table reader', () => {
    const item = chart({ data: { source: 'neige://plugin/market/history' } });
    expect(resolveLayoutData(item, () => ({ rows: [{ at: '2026-09-09', value: 10 }], caption: null })))
      .toMatchObject({ kind: 'ready', caption: '' });
  });
  it('keeps same-time observations and creates currency/null gaps without relabeling', () => {
    const item = chart({ unit: { key: 'currency', equals: 'CNY' }, data: { rows: [
      { at: '2026-09-09T00:00:00Z', value: 10, currency: 'CNY' },
      { at: '2026-09-09T00:00:00Z', value: 11, currency: 'CNY' },
      { at: '2026-09-10T00:00:00Z', value: 12, currency: 'USD' },
      { at: '2026-09-11T00:00:00Z', value: null, currency: 'CNY' },
    ] } });
    expect(layoutChartData(item, resolveLayoutData(item))).toEqual({ kind: 'ready', unit: 'CNY', points: [
      { x: Date.parse('2026-09-09'), value: 10 }, { x: Date.parse('2026-09-09'), value: 11 },
      { x: Date.parse('2026-09-10'), value: null }, { x: Date.parse('2026-09-11'), value: null },
    ] });
  });
  it('does not normalize a partial allocation to a misleading 100 percent', () => {
    const item = chart({ chart: 'donut', x: 'asset', data: { rows: [{ asset: 'A', value: 100 }, { asset: 'B', value: null }] } });
    expect(layoutChartData(item, resolveLayoutData(item)).kind).toBe('unavailable');
  });
  it('checks the total denomination before excluding the summary row', () => {
    const item = chart({ chart: 'donut', x: 'asset', exclude: { key: 'asset', value: 'Total' },
      unit: { key: 'currency', equals: 'CNY', row: { key: 'asset', value: 'Total' } },
      data: { rows: [{ asset: 'A', value: 700, currency: 'USD' }, { asset: 'Total', value: 700, currency: 'CNY' }] } });
    expect(layoutChartData(item, resolveLayoutData(item))).toEqual({ kind: 'ready', unit: 'CNY', points: [{ x: 'A', value: 700 }] });
    item.unit!.equals = 'USD';
    expect(layoutChartData(item, resolveLayoutData(item)).kind).toBe('unavailable');
  });
  it('preserves source disclosures even when source rows are invalid', () => {
    const item = chart({ data: { source: 'neige://plugin/market/history' } });
    const result = resolveLayoutData(item, () => ({ rows: [{ value: { bad: 1 } }], caption: 'Partial pricing; FX unavailable' }));
    expect(result.kind).toBe('unavailable');
    expect(result.caption).toBe('Partial pricing; FX unavailable');
  });
});

describe('annotated live tables', () => {
  it('joins by typed tuple and rejects duplicate producer identities or overwrites', () => {
    const item = table();
    item.data = { source: 'neige://plugin/market/holdings', annotations: { keys: ['asset'], rows: [{ asset: 1, name: 'Number' }] } };
    const result = resolveLayoutData(item, () => ({ rows: [{ asset: 1, value: 7 }, { asset: '1', value: 8 }] }));
    expect(result.kind === 'ready' && result.rows.map(row => row.name)).toEqual(['Number', undefined]);
    expect(resolveLayoutData(item, () => ({ rows: [{ asset: 1 }, { asset: 1 }] })).kind).toBe('unavailable');
    expect(resolveLayoutData(item, () => ({ rows: [{ asset: 1, name: 'Producer' }], caption: 'Source caption' })))
      .toMatchObject({ kind: 'unavailable', caption: 'Source caption' });
  });
  it('checks complete shares independently and never treats a display fallback as value', () => {
    const item = table();
    const column = item.columns[0];
    const read = (rows: Record<string, string | number | null>[]) => resolveLayoutData(item, () => ({ rows }));
    expect(layoutShareTotal(item, column, read([{ asset: 'A', value: 70 }, { asset: 'B', value: 30 }, { asset: 'Total', value: 100 }]))).toBe(100);
    const incomplete = read([{ asset: 'A', value: null, price: 100 }, { asset: 'Total', value: 100 }]);
    expect(layoutShareTotal(item, column, incomplete)).toBeNull();
    expect(layoutCell(column, { value: null, price: 100 }, null)).toBe('—');
    expect(layoutShareTotal(item, column, read([{ asset: 'A', value: 70 }, { asset: 'Total', value: 100 }]))).toBeNull();
    expect(layoutShareTotal(item, column, read([{ asset: 'A', value: 70 }, { asset: 'Total', value: 70 }, { asset: 'Total', value: 70 }]))).toBeNull();
  });
});

describe('configured numeric precision', () => {
  it('preserves legacy fixed digits and trims only optional trailing decimals', () => {
    const fixed = { key: 'price', label: 'Price', format: 'number' as const, digits: 8 };
    expect(layoutCell(fixed, { price: 4.637 }, null)).toBe('4.63700000');
    const flexible = { ...fixed, minDigits: 2 };
    expect(layoutCell(flexible, { price: 4.637 }, null)).toBe('4.637');
    expect(layoutCell(flexible, { price: 4.6 }, null)).toBe('4.60');
    expect(layoutCell(flexible, { price: 4 }, null)).toBe('4.00');
    expect(layoutCell(flexible, { price: 0.123456789 }, null)).toBe('0.12345679');
    expect(layoutCell(flexible, { price: null }, null)).toBe('—');
    expect(layoutCell(flexible, { price: '4.637' }, null)).toBe('—');
    expect(layoutCell({ ...flexible, minDigits: 0 }, { price: 4 }, null)).toBe('4');
    expect(layoutCell({ ...flexible, format: 'percent' }, { price: 1.2345 }, null)).toBe('1.2345%');
    expect(layoutCell({ ...flexible, format: 'share', digits: 3, minDigits: 1 }, { price: 1 }, 3)).toBe('33.333%');
    expect(layoutCell({ ...flexible, format: 'share' }, { price: 1 }, null)).toBe('—');
    expect(layoutCell({ ...flexible, format: 'text' }, { price: 4 }, null)).toBe('4');
  });

  it('accepts an optional minimum without changing or backfilling saved configuration', () => {
    const item = { ...table(), total: undefined, columns: [{ key: 'price', label: 'Price', format: 'number', digits: 8, minDigits: 2 }] };
    const accepted = parse(item);
    expect(accepted.success).toBe(true);
    if (!accepted.success) throw new Error('Expected valid minimum');
    expect(accepted.data.items[0]).toEqual(item);
    for (const digits of [0, 8]) {
      expect(parse({ ...item, columns: [{ ...item.columns[0], digits, minDigits: digits }] }).success).toBe(true);
    }
    for (const minDigits of [-1, 9, 2.5, null, '2', false]) {
      expect(parse({ ...item, columns: [{ ...item.columns[0], minDigits }] }).success).toBe(false);
    }
    expect(parse({ ...item, columns: [{ ...item.columns[0], digits: 1 }] }).success).toBe(false);
    expect(parse(table()).success).toBe(true);
  });
});
