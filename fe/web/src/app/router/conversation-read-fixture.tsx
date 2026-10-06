// Fault injection at the production router transport, shared by DOM and browser checks.
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

export function renderConversationReadFixture(initialFailure = true) {
  let unavailable = initialFailure;
  let historyUnavailable = false;
  let runGate: Promise<void> | null = null;
  const run = { card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'turn_running',
    model: null, reasoning_effort: null, blocked_reason: null, running_turn: null };
  const failure: ApiTransportResponse = { status: 503, statusText: 'Unavailable',
    body: { error: 'internal', message: 'private transport diagnostic' } };
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', reply: async (request) => {
    if (request.path.endsWith('/planner/run')) {
      await runGate;
      return unavailable ? failure : { status: 200, statusText: 'OK', body: run };
    }
    if (request.path.includes('/harness/items')) return historyUnavailable ? failure : { status: 200, statusText: 'OK', body: [{
      id: 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
      turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: 'Retained reply.' } }), created_at_ms: 1,
    }] };
    return undefined;
  } });
  return { ...fixture, pauseRun: () => {
    let release = () => {};
    runGate = new Promise<void>(resolve => { release = resolve; });
    return () => { runGate = null; release(); };
  }, failRun: (value: boolean) => { unavailable = value; },
    failHistory: (value: boolean) => { historyUnavailable = value; } };
}
