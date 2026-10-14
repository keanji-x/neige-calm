import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import type { CompactOutcome, PlannerRunTokenUsage } from '../../../../core/domain/conversation.ts';
import type { AgentProvider } from '../../../../core/api/generated/wire.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

/** Capabilities and transcript come from the real router's transport, independently of identity. */
export function renderCompactFixture(provider: AgentProvider, supported: boolean, historyReady = true, compactReply?: ApiTransportResponse) {
  const run = { card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'idle',
    model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null,
    manual_compact_supported: supported, compact_outcome: null as CompactOutcome | null, context_usage_invalidated: false, token_usage: null as PlannerRunTokenUsage | null, running_turn: null };
  const rows = [{
    id: 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
    input_segments: [{ presentation: 'user', text: 'User says:\nEarlier prompt.', attachments: [] }],
    turn_id: 'turn', turn_error_text: null, item_uuid: null, item_type: 'userMessage', method: 'item/completed',
    params: JSON.stringify({ item: { text: 'Earlier prompt.', content: [{ type: 'inputText', text: 'Earlier prompt.' }] } }), created_at_ms: 1,
  }, {
    id: 2, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
    turn_id: 'turn', turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
    params: JSON.stringify({ item: { text: 'Earlier reply.' } }), created_at_ms: 2,
  }, {
    id: 3, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
    turn_id: 'turn', turn_error_text: null, item_uuid: null, item_type: null, method: 'turn/completed',
    params: JSON.stringify({ id: 'turn', status: 'completed', error: null }), created_at_ms: 3,
  }];
  return { run, ...renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, plannerProvider: provider, reply: request => {
    if (request.path === '/api/cards/second-planner/planner/run') return { status: 200, statusText: 'OK', body: { ...run, card_id: 'second-planner', worker_session_id: 'runtime2', compact_outcome: null, context_usage_invalidated: false, token_usage: null } };
    if (request.path.endsWith('/planner/run')) return { status: 200, statusText: 'OK', body: run };
    if (request.path.endsWith('/planner/compact')) return compactReply ?? { status: 200, statusText: 'OK', body: { card_id: run.card_id, started: true } };
    if (request.path.includes('/harness/items')) return historyReady
      ? { status: 200, statusText: 'OK', body: rows }
      : { status: 503, statusText: 'Unavailable', body: { error: 'internal', message: 'History unavailable' } };
    return undefined;
  } }) };
}
