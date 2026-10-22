import { describe, expect, it } from 'vitest';
import { deriveReportOutline, readTrackReport } from './report.js';

function definition() {
  return {
    key: 'speed', title: 'Make it fast', mode: 'converge',
    checks: [{ id: 'tests', role: 'goal', config: { kind: 'command', params: { command: 'make test' } } }],
    constraints: [], budget: { turns: 10, tokens: 100000, wall_secs: 3600 }, compact: 'every_settle',
  };
}
function read(payload: unknown) {
  return readTrackReport([{
    id: 'report', track_id: 'track', kind: 'track-report', title: null, sort: 0,
    deletable: false, created_at: 0, updated_at: 0,
    payload: { blocks: [{ id: 'objective-1', kind: 'objective', payload },
      { id: 'prose-1', kind: 'prose', payload: { markdown: 'Still readable.' } }] },
  }]);
}

describe('objective report payload', () => {
  it('reads the canonical definition and uses its title in the outline', () => {
    const report = read(definition());
    expect(report?.blocks?.[0]).toEqual({ id: 'objective-1', kind: 'objective', payload: definition() });
    expect(deriveReportOutline(report?.blocks ?? null)[0]?.label).toBe('Make it fast');
  });
  it.each([
    { kind: 'metric', params: { command: 'bench', direction: 'lower', tolerance: 0.1 } },
    { kind: 'evaluator', params: { rubric: [{ id: 'clarity', criterion: 'Reads clearly' }], samples: 3, aggregate: 'majority' } },
    { kind: 'fact', params: { fact: 'pr_merged', subject: '123' } },
  ])('reads loop mode and check config $kind', config => {
    const payload = { ...definition(), mode: { loop: { plateau: 3 } },
      checks: [{ id: 'improvement', role: 'ratchet', config }] };
    expect(read(payload)?.blocks?.[0]).toEqual({ id: 'objective-1', kind: 'objective', payload });
  });
  it('reads u64 budgets above the safe integer range without silently dropping the block', () => {
    expect(read({ ...definition(), budget: { turns: 1, tokens: 1e16, wall_secs: 1 } })?.blocks?.[0]?.kind).toBe('objective');
  });
  it.each(['key', 'title', 'mode', 'checks', 'constraints', 'budget', 'compact'])('requires %s', field => {
    const payload: Record<string, unknown> = definition();
    delete payload[field];
    expect(read(payload)?.blocks?.[0]?.kind).toBe('unsupported');
  });
  it.each([
    { ...definition(), checks: [definition().checks[0], definition().checks[0]] },
    { ...definition(), mode: { loop: { plateau: 1 } } },
    { ...definition(), checks: [{ ...definition().checks[0], config: { kind: 'command', params: {} } }] },
    null, { ...definition(), title: ' ' }, { ...definition(), key: 'invalid key' },
    { ...definition(), budget: { turns: 0, tokens: 1, wall_secs: 1 } },
    { ...definition(), budget: { turns: 1, tokens: '100', wall_secs: 1 } },
    { ...definition(), budget: { turns: 1, tokens: 1 } },
    { ...definition(), checks: [] }, { ...definition(), status: 'open' },
    { ...definition(), mode: { loop: { plateau: 0 } } },
  ])('isolates malformed payload %#', payload => {
    expect(read(payload)?.blocks?.map(block => block.kind)).toEqual(['unsupported', 'prose']);
  });
});
