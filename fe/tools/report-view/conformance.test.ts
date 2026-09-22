import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { nativeViewPayloadSchema, nativeViewCanonicalSizeLowerBound } from '../../core/domain/report-view.js';
import { readTrackReport, tableBlockPayloadSchema } from '../../core/domain/report.js';
import { reportLiveViewSchema } from '../../core/domain/report-live-view.js';
import { inlineTableBlockPayloadSchema } from '../../core/domain/report-table.js';
import { z } from 'zod';

const fixture = JSON.parse(readFileSync(new URL('../../../test-data/native-view-v1.json', import.meta.url), 'utf8')) as {
  valid: Record<string, unknown>;
  canonical_sizes: { json: string; canonical: string; decoded_bytes: number; view_boundary?: boolean }[];
  budget_boundary: { rows: number; empty_canonical_bytes: number; sizes: number[] };
  invalid: { name: string; path: (string | number)[]; value?: unknown; remove?: boolean }[];
};
describe('native view conformance shared with the kernel', () => {
  it('publishes the actual generated schema without drift', () => {
    const generated = JSON.parse(readFileSync(new URL('../../core/domain/report-view.schema.json', import.meta.url), 'utf8')) as Record<string, unknown>;
    delete generated.description;
    expect(generated).toEqual(z.toJSONSchema(nativeViewPayloadSchema));
  });
  it('accepts the real App-authored portfolio example', () => {
    const example = JSON.parse(readFileSync(new URL('../../../plugins/paper-trading/examples/native-demo.json', import.meta.url), 'utf8')) as unknown;
    expect(nativeViewPayloadSchema.safeParse(example).success).toBe(true);
  });
  it('accepts all native primitives with explicit missing values', () => {
    expect(nativeViewPayloadSchema.parse(fixture.valid)).toEqual(fixture.valid);
  });
  it.each(fixture.canonical_sizes)('counts kernel layout and numeric spelling bounds for $json', value => {
    const bytes = nativeViewCanonicalSizeLowerBound(JSON.parse(value.json));
    expect(bytes).toBe(value.decoded_bytes);
    expect(bytes).toBeLessThanOrEqual(Buffer.byteLength(value.canonical));
  });
  it('rejects a compact-small table whose canonical report rendering exceeds 256 KiB', () => {
    const table = {
      columns: Array.from({ length: 32 }, (_, i) => ({ key: `c${i}`, label: `Column ${i}` })),
      rows: Array.from({ length: 500 }, () => Object.fromEntries(Array.from({ length: 32 }, (_, i) => [`c${i}`, 12345]))),
    };
    const view = { version: 1, title: '', description: '', snapshot: { id: 'size', observedAt: 0, producedAt: 0 },
      rows: [{ id: 'row', title: '', layout: 'one', cells: [{ kind: 'table', id: 'table', title: '', table }] }] };
    expect(Buffer.byteLength(JSON.stringify(view))).toBeLessThan(256 * 1024);
    expect(nativeViewPayloadSchema.safeParse(view).success).toBe(false);
  });
  it.each(fixture.budget_boundary.sizes)('enforces the shared canonical byte boundary at %i bytes', target => {
    const count = fixture.budget_boundary.rows;
    const rows = Array.from({ length: count }, () => ({ value: '' }));
    const table = { columns: [{ key: 'value', label: 'Value' }], rows };
    const view = { version: 1, title: '', description: '', snapshot: { id: 'size', observedAt: 0, producedAt: 0 },
      rows: [{ id: 'row', title: '', layout: 'one', cells: [{ kind: 'table', id: 'table', title: '', table }] }] };
    expect(nativeViewCanonicalSizeLowerBound(view)).toBe(fixture.budget_boundary.empty_canonical_bytes);
    const padding = target - fixture.budget_boundary.empty_canonical_bytes;
    for (const [index, row] of rows.entries()) row.value = 'x'.repeat(Math.floor(padding / count) + (index === 0 ? padding % count : 0));
    expect(nativeViewCanonicalSizeLowerBound(view)).toBe(target);
    expect(nativeViewPayloadSchema.safeParse(view).success).toBe(target <= 256 * 1024);
  });
  it.each(fixture.canonical_sizes.filter(value => value.view_boundary))('does not reject kernel-limit reports containing $json', scalar => {
    const count = fixture.budget_boundary.rows;
    const rows: { value: string | number }[] = Array.from({ length: count }, () => ({ value: '' }));
    rows[0].value = JSON.parse(scalar.json) as number;
    const table = { columns: [{ key: 'value', label: 'Value' }], rows };
    const view = { version: 1, title: '', description: '', snapshot: { id: 'size', observedAt: 0, producedAt: 0 },
      rows: [{ id: 'row', title: '', layout: 'one', cells: [{ kind: 'table', id: 'table', title: '', table }] }] };
    const kernelBytes = fixture.budget_boundary.empty_canonical_bytes - 2 + Buffer.byteLength(scalar.canonical);
    const padding = 256 * 1024 - kernelBytes;
    for (let index = 1; index < count; index++) rows[index].value = 'x'.repeat(Math.floor(padding / (count - 1)) + (index === 1 ? padding % (count - 1) : 0));
    expect(nativeViewCanonicalSizeLowerBound(view)).toBe(256 * 1024 - Buffer.byteLength(scalar.canonical) + scalar.decoded_bytes);
    expect(nativeViewPayloadSchema.parse(view)).toEqual(view);
  });
  it.each(Object.getOwnPropertyNames(Object.prototype))('rejects reserved table key %s without silently dropping it', key => {
    const table = { columns: [{ key, label: 'Value' }], rows: [JSON.parse(`{"${key}":"evidence"}`)] };
    expect(inlineTableBlockPayloadSchema.safeParse(table).success).toBe(false);
    expect(tableBlockPayloadSchema.safeParse(table).success).toBe(false);
    expect(reportLiveViewSchema.safeParse({ version: 1, view: 'details', title: '', table }).success).toBe(false);
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
  it('reads through the real report decoder without an app or table wrapper', () => {
    const report = readTrackReport([{ id: 'report', track_id: 't', kind: 'track-report', title: null, sort: 0,
      created_at: 0, updated_at: 0, deletable: false,
      payload: { blocks: [{ id: 'b-native', kind: 'view', payload: fixture.valid }] } }]);
    expect(report?.blocks?.[0]?.kind).toBe('view');
  });
});
