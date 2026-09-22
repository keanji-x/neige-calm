import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { nativeViewPayloadSchema } from '../../core/domain/report-view.js';
import { readTrackReport } from '../../core/domain/report.js';
import { z } from 'zod';

const fixture = JSON.parse(readFileSync(new URL('../../../test-data/native-view-v1.json', import.meta.url), 'utf8')) as {
  valid: Record<string, unknown>;
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
  it.each(fixture.invalid)('rejects $name', change => {
    const value = structuredClone(fixture.valid);
    let parent = value;
    for (const part of change.path.slice(0, -1)) parent = parent[part] as Record<string, unknown>;
    const key = change.path.at(-1)!;
    if (change.remove) delete parent[key]; else parent[key] = change.value;
    expect(nativeViewPayloadSchema.safeParse(value).success).toBe(false);
  });
  it('reads through the real report decoder without an app or table wrapper', () => {
    const report = readTrackReport([{ id: 'report', track_id: 't', kind: 'track-report', title: null, sort: 0,
      created_at: 0, updated_at: 0, deletable: false,
      payload: { blocks: [{ id: 'b-native', kind: 'view', payload: fixture.valid }] } }]);
    expect(report?.blocks?.[0]?.kind).toBe('view');
  });
});
