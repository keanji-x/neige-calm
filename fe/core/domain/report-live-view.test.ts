import { describe, expect, it } from 'vitest';
import { nativeViewPayloadSchema } from './report-view.js';
import { liveViewBlockPayloadSchema } from './report.js';

const fixture = { version: 1, title: 'Capacity', description: '',
  snapshot: { id: 'capacity', observedAt: null, producedAt: null },
  rows: [{ id: 'summary', title: '', layout: 'one', cells: [{ kind: 'metrics', id: 'size', title: '', items: [{
    id: 'measured', label: 'Storage', value: { state: 'text', text: '120 GB' }, detail: '', tone: 'neutral', emphasis: 'normal',
  }] }] }],
};
describe('one presentation grammar across live and inline delivery', () => {
  it('references source and version without prescribing an application layout', () => {
    const source = { source: 'neige://plugin/operations/capacity', version: 1 };
    expect(liveViewBlockPayloadSchema.parse(source)).toEqual(source);
    for (const value of [{ ...source, view: 'overview' }, { ...source, source: 'https://example.com' },
      { ...source, version: 2 }, { ...source, script: 'run()' }, { source: source.source }]) {
      expect(liveViewBlockPayloadSchema.safeParse(value).success).toBe(false);
    }
  });
  it('decodes the same component composition with no preset adapter', () => {
    expect(nativeViewPayloadSchema.parse(fixture)).toEqual(fixture);
    expect(nativeViewPayloadSchema.safeParse({ version: 1, view: 'overview', metrics: [] }).success).toBe(false);
  });
});
