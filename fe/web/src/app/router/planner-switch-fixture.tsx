import type { Conversation } from '../../../../core/domain/conversation.ts';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { conversationCardId, historicalConversationPage, historicalConversationRun } from './conversation-read-data.ts';

export function renderPlannerSwitchFixture(options: Readonly<{
  initial?: string;
  reply?: (request: ApiRequest) => ApiTransportResponse | Promise<ApiTransportResponse> | undefined;
  switch?: (request: ApiRequest, commit: () => void) => ApiTransportResponse | Promise<ApiTransportResponse>;
}> = {}) {
  let switched = false;
  const track = { workspace: { isolation: 'none', kind: 'attached', path: '/tmp', frozen_at: null },
    id: 'daily', area_id: 'daily-area', title: 'Switch Track', sort: 0, cwd: '/tmp', pinned_at: null,
    closed_at: null, admin_granted_at: null, created_at: 1, updated_at: 2 };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const cards = () => ['daily-planner', 'second-planner'].map((id, index) => ({ id, track_id: 'daily', kind: 'codex',
    title: index === 0 ? 'Daily Planner conversation' : 'Other conversation', sort: index,
    payload: (switched ? index === 1 : index === 0) ? { planner_harness: true } : { harness_profile: 'assistant', assistant_provider: 'codex' },
    deletable: false, created_at: 1, updated_at: 2 }));
  const list = (): Conversation[] => [{ id: switched ? 'daily-planner' : 'second-planner', trackId: 'daily',
    title: switched ? 'Daily Planner conversation' : 'Other conversation', kind: 'track-assistant',
    provider: 'codex', state: 'idle', updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null }];
  const receipt = () => ok({ track_id: 'daily', planner_card_id: 'second-planner', demoted_card_id: 'daily-planner', planner_revision: 8 });
  const fixture = renderDailyFixture({ initial: options.initial ?? '/track/daily?panel=conversations', extraPlanner: true,
    reply: async request => {
      const custom = await options.reply?.(request);
      if (custom !== undefined) return custom;
      if (request.path === '/api/tracks/daily/planner/switch') {
        const commit = () => { switched = true; };
        if (options.switch) return options.switch(request, commit);
        commit(); return receipt();
      }
      if (request.path === '/api/tracks/daily') return ok({ track, planner_revision: switched ? 8 : 7,
        can_close: false, can_reopen: false, can_admin: false, cards: cards(), overlays: [] });
      if (request.path === '/api/tracks/daily/conversations') return ok(list());
      if (request.path.includes('/planner/run?contract=conversation_v1')) return ok(historicalConversationRun({
        card_id: conversationCardId(request.path), worker_session_id: 'runtime', phase: 'idle', pending: [], pending_overflow: 0,
        model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, manual_compact_supported: true,
        recovery: null, compact_outcome: null, context_usage_invalidated: false, running_turn: null,
      }, 'idle', true));
      if (request.path.includes('/harness/items')) return ok(historicalConversationPage(conversationCardId(request.path), [{
        id: 1, worker_session_id: 'runtime', card_id: conversationCardId(request.path), track_id: 'daily', thread_id: 'thread',
        turn_id: 'turn', turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
        params: JSON.stringify({ item: { text: 'Ready for switching' } }), created_at_ms: 1,
      }], null));
      return undefined;
    } });
  return { ...fixture, commit: () => { switched = true; }, receipt, cards, list };
}
