import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { historicalConversationRun, historicalConversationPage, conversationCardId } from './conversation-read-data.ts';

export function renderCommandFixture(options: Readonly<{
  closed?: boolean; canClose?: boolean; canReopen?: boolean; running?: boolean;
  patch?: (request: ApiRequest) => ApiTransportResponse | Promise<ApiTransportResponse>;
  edit?: 'sent' | 'queued'; staleDetail?: boolean;
}> = {}) {
  let pending = options.edit === 'queued' ? [{ entry_id: 'queued', text: 'Original queued input', rev: 2, queued_at_ms: 1 }] : [];
  let replaced = false;
  let closed = options.closed ?? false;
  let track = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null },
    id: 'daily', area_id: 'daily-area', title: 'Command Track', sort: 0, cwd: '/tmp', pinned_at: null,
    closed_at: closed ? 1 : null, admin_granted_at: null, created_at: 1, updated_at: 2 };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  return renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, reply: async request => {
    if (request.method === 'PATCH' && request.path.endsWith('/planner/input/queued')) {
      pending = [{ ...pending[0], text: (request.body as { text: string }).text, rev: 3 }];
      return ok({ ...pending[0], card_id: 'daily-planner' });
    }
    if (request.method === 'POST' && request.path.endsWith('/planner/input')) {
      replaced = true;
      return ok({ card_id: 'daily-planner', worker_session_id: 'runtime' });
    }
    if (request.method === 'PATCH' && request.path === '/api/tracks/daily') {
      if (options.patch !== undefined) return options.patch(request);
      if (!options.staleDetail) {
        const body = request.body as { closed?: boolean };
        closed = body.closed ?? closed;
        track = { ...track, closed_at: closed ? 1 : null };
      }
      return ok(track);
    }
    if (request.path === '/api/tracks/daily') return ok({ track,
      can_reopen: options.canReopen ?? closed, can_close: options.canClose ?? !closed, can_admin: true,
      cards: [{ id: 'daily-planner', track_id: 'daily', kind: 'codex', title: 'Daily Planner conversation', sort: 1,
        payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 }], overlays: [] });
    if (request.path.includes('/harness/items') && options.edit === 'sent') {
      const row = { worker_session_id: 'runtime', card_id: conversationCardId(request.path), track_id: 'daily', thread_id: 'thread',
        turn_id: 'turn', turn_error_text: null, item_uuid: null, created_at_ms: 1 };
      return ok(historicalConversationPage(conversationCardId(request.path), replaced ? [] : [
        { ...row, id: 1, item_type: 'userMessage', method: 'item/completed',
          input_segments: [{ presentation: 'user', text: 'User says:\nOriginal prompt', attachments: [] }],
          params: JSON.stringify({ item: { content: [{ text: 'Original prompt' }] } }) },
        { ...row, id: 2, item_type: 'agentMessage', method: 'item/completed', params: JSON.stringify({ item: { text: 'Ready for commands' } }) },
        { ...row, id: 3, item_type: null, method: 'turn/completed', params: JSON.stringify({ id: 'turn', status: 'completed', error: null }) },
      ], null));
    }
    if (request.path.includes('/harness/items')) return ok(historicalConversationPage(conversationCardId(request.path), [{
      id: 1, worker_session_id: 'runtime', card_id: conversationCardId(request.path), track_id: 'daily', thread_id: 'thread',
      turn_id: 'turn', turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: 'Ready for commands' } }), created_at_ms: 1,
    }], null));
    if (request.path.includes('/planner/run?contract=conversation_v1')) return ok(historicalConversationRun({
      card_id: request.path.split('/')[3], worker_session_id: 'runtime', phase: options.running ? 'turn_running' : 'idle',
      pending, pending_overflow: 0, model: null, reasoning_effort: null, permission_mode: conversationCardId(request.path) === 'daily-planner' ? 'never' : null, blocked_reason: null, manual_compact_supported: true,
      recovery: null, compact_outcome: null, context_usage_invalidated: false, running_turn: null,
    }, 'idle', true));
    if (request.path.endsWith('/planner/interrupt')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', stopped: true });
    if (request.path.endsWith('/planner/input')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime' });
    return undefined;
  } });
}
