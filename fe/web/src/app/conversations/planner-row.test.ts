import { expect, it } from 'vitest';
import type { CardWire } from '../../../../core/domain/track.ts';
import { trackConversationRows } from './planner-row.ts';

function worker(overrides: Partial<CardWire> = {}): CardWire {
  return { id: 'worker', track_id: 'track', kind: 'codex', title: null, sort: 1, deletable: true,
    created_at: 1, updated_at: 2, payload: { harness_profile: 'task_worker', worker_provider: 'opencode' }, ...overrides };
}

it('projects a terminal-free worker using its declared provider and read-only access', () => {
  const [row] = trackConversationRows('track', 'Track', [worker()], []);
  expect(row).toMatchObject({ id: 'worker', provider: 'opencode', readOnly: true, title: 'Task worker' });
});

it('uses managed-worker runtime metadata without depending on a terminal id', () => {
  const [row] = trackConversationRows('track', 'Track', [worker({ payload: null,
    runtime: { worker_session_id: 'runtime', kind: 'managed-worker', provider: 'grok', status: 'running', updated_at_ms: 3 } })], []);
  expect(row).toMatchObject({ id: 'worker', provider: 'grok', readOnly: true, state: 'running' });
});

it('does not invent a provider when a worker lacks its declared selection', () => {
  expect(trackConversationRows('track', 'Track', [worker({ payload: { harness_profile: 'task_worker' } })], [])).toEqual([]);
});
