import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { ConversationInitialInputObservation, HarnessItem, TrackConversationSummary } from '../../../../core/api/generated/wire.ts';
import type { PendingQueueEntry } from '../../../../core/domain/conversation.ts';
import { sha256Hex } from '../../../../core/domain/sha256.ts';
import { HARNESS_ITEMS_PAGE_LIMIT, trackConversationCardId } from '../../../../core/domain/conversation.ts';
import { initialReceiptFor } from './initial-input-fixture.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { useLayoutEffect } from 'react';
import { useConversationCommands, type ConversationRegistry } from '../conversations/public.tsx';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
export const FIRST_MESSAGE = 'Keep the first sentence readable through adoption.';

/** The real router, decoder, outbox, queries and presentation over a controllable serialized server result. */
export function renderFirstMessageFixture(options: { onCommit?: () => void; capability?: boolean; loseReply?: boolean;
  incomplete?: boolean; legacy?: boolean; mobile?: boolean; holdHistory?: boolean; holdRun?: boolean; harvestedInput?: boolean; running?: boolean; refuseQueueDelete?: boolean } = {}) {
  let original: ApiRequest | null = null;
  let created: TrackConversationSummary | null = null;
  let history: HarnessItem[] = [];
  let earlierHistory: HarnessItem[] = [];
  let observation: ConversationInitialInputObservation | null = null;
  let replyCreate: (() => void) | null = null;
  let replyInspect: (() => void) | null = null;
  let inspecting = 0;
  const heldCreate = new Promise<void>(resolve => { replyCreate = resolve; });
  const heldInspect = new Promise<void>(resolve => { replyInspect = resolve; });
  let failHistory = false;
  let failRun = false;
  const heldHistory: (() => void)[] = [];
  const heldRun: (() => void)[] = [];
  const heldList: (() => void)[] = [];
  let holdNextList = false;
  const heldInspections: (() => void)[] = [];
  let holdNextInspection = false;
  let pending: PendingQueueEntry[] = [];
  let registry: ConversationRegistry | null = null;
  const staleRevision = (request: ApiRequest, entry: PendingQueueEntry): ApiTransportResponse | null => {
    const revision = (request.body as { if_entry_rev: unknown }).if_entry_rev;
    if (typeof revision !== 'number') throw new Error('Queue write lacks its declared revision.');
    return revision === entry.rev ? null : { status: 409, statusText: 'Conflict', body: {
      code: 'planner_input_stale', error: 'Entry changed.', entry_id: entry.entry_id, text: entry.text, rev: entry.rev,
    } };
  };
  function RegistryProbe() {
    const current = useConversationCommands();
    useLayoutEffect(() => { registry = current; });
    return null;
  }
  const view = renderDailyFixture({ initial: `/track/daily${options.mobile ? '' : '?panel=conversations'}`, onCommit: options.onCommit, routeProbe: <RegistryProbe />,
    reply: async request => {
      if (request.path === '/api/version') return ok({ assistantProviders: true, conversationInitialInput: options.capability ?? true,
        conversationSide: true, conversationCreateModel: true, webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
        syncEventVersion: 29, dbInstanceId: 'first-message-continuity' });
      if (request.path.startsWith('/api/agent-providers/managed')) return ok([{
        availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
        capabilities: { managed_conversation: true, steer: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false },
      }]);
      if (request.path === '/api/tracks/daily/conversations') {
        if (request.method === 'POST') {
          original = request;
          const key = request.headers?.['Idempotency-Key'];
          if (key === undefined) throw new Error('Missing original key');
          created = { id: trackConversationCardId('daily', key), trackId: 'daily', title: null, kind: 'track-assistant', provider: 'codex',
            state: options.incomplete ? 'failed' : 'idle', updatedAt: 10, archivedAt: null, lastTurnCompletedAt: null };
          await heldCreate;
          if (options.loseReply) throw new Error('The create reply was lost.');
          return { status: 201, statusText: 'Created', body: { ...created, initialInput: options.legacy ? { kind: 'legacy-unavailable' }
            : { kind: 'known', receipt: initialReceiptFor(request, 'first-entry', 0) } } };
        }
        const result = ok(created === null ? [] : [created]);
        if (holdNextList) {
          holdNextList = false;
          return await new Promise<ApiTransportResponse>(resolve => { heldList.push(() => { resolve(result); }); });
        }
        return result;
      }
      if (request.path.endsWith('/initial-input')) {
        inspecting += 1;
        if (original === null || request.headers?.['Idempotency-Key'] !== original.headers?.['Idempotency-Key']) throw new Error('Wrong creation key');
        if (options.incomplete) return ok({ kind: 'unavailable', reason: 'creation-incomplete', receipt: null });
        await heldInspect;
        if (observation === null) throw new Error('No coherent observation supplied');
        const result = ok(observation);
        if (holdNextInspection) {
          holdNextInspection = false;
          return await new Promise<ApiTransportResponse>(resolve => { heldInspections.push(() => { resolve(result); }); });
        }
        return result;
      }
      if (created !== null && request.path.includes(`/${created.id}/harness/items`)) {
        const cursor = new URL(request.path, 'http://fixture.test').searchParams.get('after_id');
        if (cursor === null) throw new Error('The history request lacks its declared cursor.');
        const page = Number(cursor) > 0 ? earlierHistory.filter(row => row.id < Number(cursor)) : history;
        const result = failHistory ? { status: 500, statusText: 'Error', body: { error: 'History unavailable.' } } : ok(page);
        if (options.holdHistory) return await new Promise<ApiTransportResponse>(resolve => { heldHistory.push(() => { resolve(result); }); });
        return result;
      }
      if (request.method === 'POST' && request.path.endsWith('/planner/input')) {
        const tail = pending.find(entry => entry.entry_id === 'ordinary-entry');
        if (tail !== undefined) {
          const text = (request.body as { text: unknown }).text;
          if (typeof text !== 'string') throw new Error('Ordinary fold lacks actual submitted text.');
          pending = pending.map(entry => entry === tail ? { ...entry, rev: entry.rev + 1, text: `${entry.text}\n\n${text}` } : entry);
        }
        return ok({ card_id: request.path.split('/')[3], worker_session_id: 'runtime', entry_id: 'ordinary-entry' });
      }
      if (options.refuseQueueDelete && request.method === 'DELETE' && request.path.endsWith('/planner/input/first-entry')) {
        return { status: 409, statusText: 'Conflict', body: { code: 'planner_input_stale', error: 'Entry changed.',
          entry_id: 'first-entry', text: 'Changed elsewhere.', rev: 1 } };
      }
      if (request.method === 'DELETE' && request.path.endsWith('/planner/input/first-entry')) {
        const entry = pending.find(entry => entry.entry_id === 'first-entry');
        if (entry === undefined) throw new Error('No actual first entry to delete.');
        const refused = staleRevision(request, entry); if (refused !== null) return refused;
        pending = pending.filter(candidate => candidate.entry_id !== entry.entry_id);
        return ok({ card_id: created?.id, worker_session_id: 'runtime', entry_id: entry.entry_id, rev: entry.rev, text: null });
      }
      if (request.method === 'PATCH' && request.path.endsWith('/planner/input/first-entry')) {
        const entry = pending.find(entry => entry.entry_id === 'first-entry');
        const text = (request.body as { text?: unknown }).text;
        if (entry === undefined || typeof text !== 'string') throw new Error('No actual queued edit.');
        const refused = staleRevision(request, entry); if (refused !== null) return refused;
        const saved = { ...entry, text, rev: entry.rev + 1 };
        pending = pending.map(candidate => candidate.entry_id === entry.entry_id ? saved : candidate);
        return ok({ card_id: created?.id, worker_session_id: 'runtime', entry_id: saved.entry_id, rev: saved.rev, text: saved.text });
      }
      if (request.method === 'POST' && request.path.endsWith('/planner/input/first-entry/steer')) {
        const entry = pending.find(entry => entry.entry_id === 'first-entry');
        if (entry === undefined) throw new Error('No actual first entry to steer.');
        const refused = staleRevision(request, entry); if (refused !== null) return refused;
        pending = pending.filter(candidate => candidate.entry_id !== entry.entry_id);
        return ok({ card_id: created?.id, entry_id: entry.entry_id, steered: true, turn_id: 'running-first-input' });
      }
      if (request.path.endsWith('/planner/run')) {
        if (failRun && created !== null && request.path.includes(`/${created.id}/`)) return { status: 500, statusText: 'Error', body: { error: 'Run unavailable.' } };
        const result = ok({ card_id: request.path.split('/')[3], worker_session_id: 'runtime',
        phase: options.running ? 'turn_running' : 'idle', model: null, reasoning_effort: null, permission_mode: null, blocked_reason: null,
        manual_compact_supported: false, recovery: null, compact_outcome: null, context_usage_invalidated: false, running_turn: options.running ? { turn_id: 'running-first-input', elapsed_ms: 1 } : null, pending, pending_overflow: 0 });
        if (options.holdRun && created !== null && request.path.includes(`/${created.id}/`)) {
          return await new Promise<ApiTransportResponse>(resolve => { heldRun.push(() => { resolve(result); }); });
        }
        return result;
      }
      return undefined;
    },
  });
  const receipt = () => {
    if (original === null) throw new Error('No request dispatched');
    return initialReceiptFor(original, 'first-entry', 0);
  };
  return { ...view,
    createdId: () => created?.id ?? null,
    inspectionCount: () => inspecting,
    holdNextInspection: () => { holdNextInspection = true; },
    heldInspectionCount: () => heldInspections.length,
    releaseInspection: (index: number) => { heldInspections[index](); },
    answerCreate: () => { replyCreate?.(); },
    observe: (kind: 'queued' | 'changed' | 'projected' | 'unavailable') => {
      const binding = receipt();
      if (kind === 'projected') {
        const source = [...history, ...earlierHistory].find(row => row.id === 7);
        const ref = source?.input_segments?.find(segment => segment.origin?.queue_entry?.id === 'first-entry')?.origin?.queue_entry;
        if (ref == null || source?.turn_id == null) throw new Error('Projected response requires an actual versioned provider-bound source.');
        observation = { kind, receipt: binding, runtimeId: 'runtime', entryRev: ref.rev, itemId: 7,
          segmentIndex: options.harvestedInput === false ? 0 : 1 };
        replyInspect?.(); return;
      }
      observation = kind === 'queued' || kind === 'changed' ? { kind: 'queued', receipt: binding, runtimeId: 'runtime', entryId: binding.entryId,
        rev: kind === 'changed' ? 1 : 0, text: kind === 'changed' ? 'Changed by another client.' : FIRST_MESSAGE,
        textSha256: kind === 'changed' ? sha256Hex('Changed by another client.') : binding.textSha256 }
        : { kind, receipt: binding, reason: 'runtime-unavailable' };
      replyInspect?.();
    },
    failRun: () => { failRun = true; },
    failHistory: () => { failHistory = true; },
    recoverHistory: () => { failHistory = false; },
    historyCount: () => heldHistory.length,
    releaseHistory: (index: number) => { heldHistory[index](); },
    clearHistory: () => { history = []; earlierHistory = []; },
    advanceHistoryWindow: () => {
      if (created === null) throw new Error('No created conversation for a later page.');
      const cardId = created.id; earlierHistory = history;
      history = Array.from({ length: HARNESS_ITEMS_PAGE_LIMIT }, (_, index) => ({ id: 20 + index, card_id: cardId,
        worker_session_id: 'runtime', track_id: 'daily', thread_id: 'thread', turn_id: 'later-turn',
        item_uuid: `later-item-${index}`, item_type: 'agentMessage', method: 'item/completed',
        params: JSON.stringify({ item: { text: `Later page reply ${index}.` } }), created_at_ms: 20 + index, turn_error_text: null }));
    },
    outbox: () => created === null ? [] : registry?.outboxOf(created.id) ?? [],
    queueOrdinary: (text: string, rev: number) => { pending = [{ entry_id: 'ordinary-entry', text, queued_at_ms: 101, rev }]; },
    projectOrdinary: (entryRev: number, text: string, provisional: boolean) => {
      if (created === null) throw new Error('No canonical conversation');
      const id = 9 + entryRev;
      history = [...history.filter(row => row.id !== id), { id, card_id: created.id, worker_session_id: 'runtime', track_id: 'daily',
        thread_id: 'thread', turn_id: provisional ? null : 'ordinary-provider-turn', item_uuid: `ordinary-source-${entryRev}`,
        item_type: 'userMessage', method: 'item/completed', params: provisional ? JSON.stringify({ _projection: true }) : '{}',
        created_at_ms: 9 + entryRev, turn_error_text: null, input_segments: [{ presentation: 'user', text, attachments: [],
          origin: { observation: 'user', event_id: null, queue_entry: { id: 'ordinary-entry', rev: entryRev } } }] }];
    },
    queueOriginal: () => { pending = [{ entry_id: 'first-entry', text: FIRST_MESSAGE, queued_at_ms: 100, rev: 0 }]; },
    queueEdited: (text: string) => { pending = [{ entry_id: 'first-entry', text, queued_at_ms: 100, rev: 1 }]; },
    queueAtRevision: (text: string, rev: number) => { pending = [{ entry_id: 'first-entry', text, queued_at_ms: 100, rev }]; },
    clearQueue: () => { pending = []; },
    runCount: () => heldRun.length,
    releaseRun: (index: number) => { heldRun[index](); },
    holdNextList: () => { holdNextList = true; },
    listCount: () => heldList.length,
    releaseList: (index: number) => { heldList[index](); },
    project: (entryRev: number, text = FIRST_MESSAGE, provisional = false) => {
      if (created === null) throw new Error('No canonical conversation');
      history = [{ id: 7, card_id: created.id, worker_session_id: 'runtime', track_id: 'daily', thread_id: 'thread', turn_id: provisional ? null : 'accepted-first-turn',
        item_uuid: options.harvestedInput === false ? 'first-entry' : 'harvested-entry', item_type: 'userMessage', method: 'item/completed', params: provisional ? JSON.stringify({ _projection: true }) : '{}', created_at_ms: 8, turn_error_text: null,
        input_segments: [
          ...(options.harvestedInput === false ? [] : [{ presentation: 'user' as const, text: 'Harvested before first input', attachments: [],
            origin: { observation: 'user', event_id: null, queue_entry: { id: 'harvested-entry', rev: 0 } } }]),
          { presentation: 'user', text, attachments: [], origin: { observation: 'user', event_id: null, queue_entry: { id: 'first-entry', rev: entryRev } } },
        ] }];
    },
  };
}
