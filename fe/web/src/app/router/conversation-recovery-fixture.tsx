import { initialReceiptFor } from './initial-input-fixture.ts';
import { useLayoutEffect } from 'react';
import { useConversationDraft } from '../conversations/public.tsx';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { trackConversationCardId } from '../../../../core/domain/conversation.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import type { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
export const RECOVERY_TEXT = 'Keep this message through recovery.';

/** API-only faults; the router, outbox, read ordering and focus are production code. */
export function renderRecoveryFixture({ accepted = false, holdReads = false, storeMessage = true, capabilitiesUnavailable = false, strict = false, recovery, createFailure, onDraftCommit }: {
  accepted?: boolean; holdReads?: boolean; storeMessage?: boolean; capabilitiesUnavailable?: boolean; strict?: boolean; recovery?: RecoveryAccess; createFailure?: ApiTransportResponse; onDraftCommit?: (key: string | null) => void;
} = {}) {
  let allowSend = accepted;
  let sent = false;
  let sentText = RECOVERY_TEXT;
  let failedReads = false;
  let holdPost = false;
  let created: { id: string; trackId: string; title: null; kind: string; provider: string; state: null; updatedAt: number; archivedAt: null, lastTurnCompletedAt: null } | null = null;
  const heldReads: (() => void)[] = [];
  const heldPosts: (() => void)[] = [];
  let holdCapabilities = false;
  const heldCapabilities: (() => void)[] = [];
  const row = (cardId: string, user = false) => ({ id: user ? 2 : 1, card_id: cardId, track_id: 'daily', worker_session_id: 'runtime',
    thread_id: 'thread', turn_id: null, turn_error_text: null, item_uuid: null, item_type: user ? 'userMessage' : 'agentMessage',
    method: 'item/completed', params: JSON.stringify({ item: user ? { content: [{ text: sentText }] } : { text: 'A retained response.' } }), created_at_ms: user ? 2 : 1 });
  const read = (body: () => unknown): ApiTransportResponse | Promise<ApiTransportResponse> => {
    const answer = () => failedReads ? { status: 500, statusText: 'Error', body: { error: 'Conversation refresh is unavailable.' } } : ok(body());
    return sent && holdReads ? new Promise(resolve => { heldReads.push(() => resolve(answer())); }) : answer();
  };
  function DraftCommitProbe() {
    const key = useConversationDraft('daily')?.key ?? null;
    useLayoutEffect(() => { onDraftCommit?.(key); }, [key]);
    return null;
  }
  const view = renderDailyFixture({ strict, routeProbe: onDraftCommit === undefined ? undefined : <DraftCommitProbe />, withTransport: recovery === undefined ? undefined : transport => createRecoveryTransports(transport, recovery).business,
    initial: '/track/daily?panel=conversations', extraPlanner: true, reply: async request => {
    if (request.path === '/api/version') return ok({ assistantProviders: true, conversationInitialInput: true, conversationCreateModel: true,
      webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 29, dbInstanceId: 'recovery-feedback' });
    if (request.path.startsWith('/api/agent-providers/managed')) {
      if (capabilitiesUnavailable) return { status: 500, statusText: 'Error', body: { error: 'Capabilities unavailable.' } };
      if (holdCapabilities) await new Promise<void>(finish => { heldCapabilities.push(finish); });
      return ok([{
      availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      capabilities: { managed_conversation: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false },
      }]);
    }
    if (request.method === 'POST' && (request.path.endsWith('/planner/input') || request.path.endsWith('/conversations'))) {
      if (!allowSend) {
        if (request.path.endsWith('/conversations') && createFailure !== undefined) return createFailure;
        throw new Error('The send response was lost.');
      }
      if (holdPost) await new Promise<void>(finish => { heldPosts.push(finish); });
      if (request.body === null || typeof request.body !== 'object' || !('text' in request.body) || typeof request.body.text !== 'string') throw new Error('A send must carry text.');
      sentText = request.body.text;
      sent = true;
      if (request.path.endsWith('/conversations')) {
        const key = request.headers?.['Idempotency-Key'];
        if (key === undefined) throw new Error('A create must carry its original key.');
        created = { id: trackConversationCardId('daily', key), trackId: 'daily', title: null, kind: 'track-assistant', provider: 'codex', state: null, updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null };
        return { status: 201, statusText: 'Created', body: { ...created, initialInput: { kind: 'known', receipt: initialReceiptFor(request, 'first-entry', 0) } } };
      }
      return ok({ card_id: 'second-planner', worker_session_id: 'runtime' });
    }
    if (created !== null && request.path.endsWith('/initial-input')) {
      const post = view.requests.find(candidate => candidate.method === 'POST' && candidate.path.endsWith('/conversations')
        && candidate.headers?.['Idempotency-Key'] === request.headers?.['Idempotency-Key']);
      if (post === undefined) throw new Error('No original creation request');
      return ok({ kind: 'projected', entryRev: 0, receipt: initialReceiptFor(post, 'first-entry', 0), runtimeId: 'runtime', itemId: 2, segmentIndex: 0 });
    }
    if (created !== null && request.path === '/api/tracks/daily/conversations') return ok([created]);
    if (request.path.includes('/planner/run?contract=conversation_v1')) return read(() => ({ card_id: created?.id ?? 'second-planner', worker_session_id: 'runtime', phase: 'idle',
      model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, manual_compact_supported: true,
      compact_outcome: null, context_usage_invalidated: false, running_turn: null }));
    if (request.path.includes('/harness/items')) {
      const cardId = created?.id ?? 'second-planner';
      return read(() => created !== null ? [{ ...row(cardId, true), turn_id: 'accepted-initial-turn', input_segments: [{
        presentation: 'user', text: sentText, attachments: [], origin: { observation: 'user_message', event_id: null, queue_entry: { id: 'first-entry', rev: 0 } },
      }] }] : [row(cardId), ...(sent && storeMessage ? [row(cardId, true)] : [])]);
    }
    return undefined;
  } });
  return { ...view,
    allow: () => { allowSend = true; },
    pauseSend: () => { holdPost = true; },
    postHeld: () => heldPosts.length,
    createdId: () => created?.id,
    recoverCapabilities: () => { capabilitiesUnavailable = false; holdCapabilities = true; },
    capabilitiesHeld: () => heldCapabilities.length,
    heldCount: () => heldReads.length,
    failReads: () => { failedReads = true; },
    store: () => { storeMessage = true; failedReads = false; },
    release: () => {
      holdReads = false; holdPost = false; holdCapabilities = false;
      for (const finish of [...heldPosts.splice(0), ...heldReads.splice(0), ...heldCapabilities.splice(0)]) finish();
    },
  };
}
