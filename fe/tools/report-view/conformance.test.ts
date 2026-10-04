import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { nativeViewPayloadSchema } from '../../core/domain/report-view.js';
import { LIVE_TABLE_SOURCE_PATTERN, readTrackReport, tableBlockPayloadSchema } from '../../core/domain/report.js';
import { inlineTableBlockPayloadSchema } from '../../core/domain/report-table.js';

const fixture = JSON.parse(readFileSync(new URL('../../../test-data/native-view-v1.json', import.meta.url), 'utf8')) as {
  valid: Record<string, unknown>;
  valid_slots: Record<string, unknown>;
  wide_layouts: string[];
  neutral_palette: number;
  canonical_sizes: { json: string; canonical: string; decoded_bytes: number; view_boundary?: boolean }[];
  budget_boundary: { rows: number; empty_canonical_bytes: number; sizes: number[] };
  invalid: { name: string; path: (string | number)[]; value?: unknown; remove?: boolean }[];
};
describe('native view conformance shared with the kernel', () => {
  it('accepts the shared neutral palette', () => {
    const view = nativeViewPayloadSchema.parse(fixture.valid);
    const chart = view.rows[0].cells[1];
    if (chart.kind !== 'time-series') throw new Error('Expected chart fixture');
    chart.datasets[0].series[0].palette = fixture.neutral_palette;
    expect(nativeViewPayloadSchema.safeParse(view).success).toBe(true);
  });
  it.each(fixture.wide_layouts)('accepts explicit ratio %s only with two cells', layout => {
    const view = structuredClone(fixture.valid) as { rows: { layout: string; cells: unknown[] }[] };
    view.rows[0].layout = layout;
    expect(nativeViewPayloadSchema.safeParse(view).success).toBe(true);
    view.rows[0].cells.pop();
    expect(nativeViewPayloadSchema.safeParse(view).success).toBe(false);
  });
  it('uses the crate-owned presentation contract without backend imports of frontend files', () => {
    const contract = JSON.parse(readFileSync(new URL('../../../crates/calm-types/src/report_blocks/native_view.schema.json', import.meta.url), 'utf8')) as { $defs: Record<string, unknown> };
    expect(contract.$defs.NativeView).toBeDefined();
    const backend = readFileSync(new URL('../../../crates/calm-server/src/mcp_server/tools/track_report_blocks/contracts.rs', import.meta.url), 'utf8');
    expect(backend).toContain('report_blocks::native_view::schema()');
    expect(backend).not.toContain('/../../fe/');
  });
  it('accepts the shared live-slot template', () => {
    expect(nativeViewPayloadSchema.parse(fixture.valid_slots)).toEqual(fixture.valid_slots);
  });
  it('compiles the same live source rule the live table and chart.series decoders use', () => {
    const contract = JSON.parse(readFileSync(new URL('../../../crates/calm-types/src/report_blocks/native_view.schema.json', import.meta.url), 'utf8')) as {
      $defs: { LiveSlot: { properties: { source: { pattern: string; maxLength: number } } } };
    };
    const source = contract.$defs.LiveSlot.properties.source;
    // RegExp#source escapes `/`, so compare compiled patterns, not the raw schema string.
    expect(new RegExp(source.pattern).source).toBe(LIVE_TABLE_SOURCE_PATTERN.source);
    expect(source.maxLength).toBe(2048);
  });
  it('accepts the real App-authored portfolio example', () => {
    const example = JSON.parse(readFileSync(new URL('../../../plugins/paper-trading/examples/native-demo.json', import.meta.url), 'utf8')) as unknown;
    expect(nativeViewPayloadSchema.safeParse(example).success).toBe(true);
  });
  it('accepts all native primitives with explicit missing values', () => {
    expect(nativeViewPayloadSchema.parse(fixture.valid)).toEqual(fixture.valid);
  });
  it('uses an independent read budget rather than guessing canonical write admission', () => {
    const table = {
      columns: Array.from({ length: 32 }, (_, i) => ({ key: `c${i}`, label: `Column ${i}` })),
      rows: Array.from({ length: 500 }, () => Object.fromEntries(Array.from({ length: 32 }, (_, i) => [`c${i}`, 12345]))),
    };
    const view = { version: 1, title: '', description: '', snapshot: { id: 'size', observedAt: null, producedAt: null },
      rows: [{ id: 'row', title: '', layout: 'one', cells: [{ kind: 'table', id: 'table', title: '', table }] }] };
    expect(nativeViewPayloadSchema.safeParse(view).success).toBe(true);
  });
  it.each(Object.getOwnPropertyNames(Object.prototype))('rejects reserved table key %s without silently dropping it', key => {
    const table = { columns: [{ key, label: 'Value' }], rows: [JSON.parse(`{"${key}":"evidence"}`)] };
    expect(inlineTableBlockPayloadSchema.safeParse(table).success).toBe(false);
    expect(tableBlockPayloadSchema.safeParse(table).success).toBe(false);
    const view = { version: 1, title: '', description: '', snapshot: { id: 'keys', observedAt: null, producedAt: null },
      rows: [{ id: 'row', title: '', layout: 'one', cells: [{ kind: 'table', id: 'table', title: '', table }] }] };
    expect(nativeViewPayloadSchema.safeParse(view).success).toBe(false);
  });
  it('rejects an undeclared prototype key before record decoding can erase it', () => {
    const table = { columns: [{ key: 'value', label: 'Value' }], rows: [JSON.parse('{"value":1,"__proto__":"hidden"}')] };
    expect(inlineTableBlockPayloadSchema.safeParse(table).success).toBe(false);
  });
  it.each(fixture.invalid)('rejects $name', change => {
    const value = structuredClone(fixture.valid);
    let parent = value;
    for (const part of change.path.slice(0, -1)) parent = parent[part] as Record<string, unknown>;
    const key = change.path.at(-1)!;
    if (change.remove) delete parent[key];
    else Object.defineProperty(parent, key, { value: change.value, writable: true, enumerable: true, configurable: true });
    expect(nativeViewPayloadSchema.safeParse(value).success).toBe(false);
  });
  it.each([-1e15, 1e15])('accepts inclusive signed observation boundary %s', boundary => {
    const value = structuredClone(fixture.valid) as { rows: { cells: { datasets: { points: { values: (number | null)[] }[] }[] }[] }[] };
    value.rows[0].cells[1].datasets[0].points[0].values[0] = boundary;
    expect(nativeViewPayloadSchema.safeParse(value).success).toBe(true);
  });
  it('reads through the real report decoder without an app or table wrapper', () => {
    const report = readTrackReport([{ id: 'report', track_id: 't', kind: 'track-report', title: null, sort: 0,
      created_at: 0, updated_at: 0, deletable: false,
      payload: { blocks: [{ id: 'b-native', kind: 'view', payload: fixture.valid }] } }]);
    expect(report?.blocks?.[0]?.kind).toBe('view');
  });
});
