// @vitest-environment jsdom
// Starting a conversation on a track, driven through the real router; and what the
// session registry may remember about one, driven through the real store.

import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useEffect } from 'react';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import { applyEventEffects } from '../events/query-invalidation-adapter.ts';
import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { trackConversationCardId } from '../../../../core/domain/conversation.ts';
import { ConversationProvider, useConversationRegistry } from '../conversations/public.tsx';
import { createUiPreferences, type UiPreferenceStorage } from '../providers/ui-preferences.tsx';
import { DATABASE_ID_KEY } from '../../../../core/keys/storage.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter, useConversationStore } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = { id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
/* A track with no planner card. */
const BARE_TRACK = { ...TRACK, id: 'w2', title: 'Bare track', sort: 2 };
const PLANNER_CARD = { id: 'card-planner', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1, payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
/* The card an assistant conversation is: a codex card carrying the marker the
   kernel persists (`plain_chat.rs::card_is_track_assistant`). */
const ASSISTANT_CARD = { ...PLANNER_CARD, id: 'conv-assistant-1', title: null, payload: { harness_profile: 'assistant' }, sort: 2, updated_at: 30 };
/* A worker card, so the CARDS panel has one thing to list. */
const WORKER_CARD = { ...PLANNER_CARD, id: 'card-worker', title: 'Worker', payload: {}, sort: 3, updated_at: 4 };
/* The kernel's `kernel/track/activity` overlay: `items` are what the aside lists,
 * `cards` the per-card verdicts every row and card head reads. */
type ActivityItemWire = {
  kind: 'input' | 'failed'; source: 'card' | 'task' | 'session' | 'lifecycle';
  id: string; card_id: string | null; at_ms: number;
};
type ActivityCardWire = { card_id: string; state: 'working' | 'input' | 'failed' };
const trackActivityOverlay = (payload: Partial<{
  working: boolean; attention: 'none' | 'input' | 'failed'; activity_at_ms: number | null;
  items: ActivityItemWire[]; cards: ActivityCardWire[];
}> = {}, trackId = 'w1') => ({
  id: `activity-${trackId}`, plugin_id: 'kernel', entity_kind: 'track', entity_id: trackId, kind: 'activity',
  payload: { schemaVersion: 1, working: false, attention: 'none', activity_at_ms: null, items: [], cards: [], ...payload },
  updated_at: 3,
});
/** A card's own input request, as the projector lists it: one item and one card verdict. */
const cardInputItem = (cardId: string, kind: 'input' | 'failed', atMs: number): ActivityItemWire =>
  ({ kind, source: 'card', id: cardId, card_id: cardId, at_ms: atMs });
/** The retired per-card status row (`kernel/card/status`): nothing reads it any more. */
const cardStatusOverlay = (cardId: string, state: 'AwaitingInput' | 'Errored', updatedAt: number) => ({
  id: `status-${cardId}`, plugin_id: 'kernel', entity_kind: 'card', entity_id: cardId,
  kind: 'status', payload: { state }, updated_at: updatedAt,
});

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

const CONVERSATIONS = '/api/tracks/w1/conversations';
const BARE_CONVERSATIONS = '/api/tracks/w2/conversations';
const HISTORY_PATH = '/harness/items';

type Row = {
  id: string; trackId: string; title: string | null; kind: string;
  state: string | null; updatedAt: number; lastTurnCompletedAt: number | null;
};

function assistantRow(overrides: Partial<Row> = {}): Row {
  return {
    id: ASSISTANT_CARD.id, trackId: 'w1', title: null, kind: 'track-assistant',
    state: 'idle', updatedAt: 30, lastTurnCompletedAt: null, ...overrides,
  };
}

/* Read receipts need a database scope; without `ServerCompatGate` it is seeded
 * from the stored database identity. Under a null scope nothing is unread. */
function receiptStorage(): UiPreferenceStorage {
  const values = new Map<string, string>([[DATABASE_ID_KEY, 'db-receipts']]);
  return { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => { values.set(key, value); } };
}

/** One persisted transcript row. An agent message has `text`, a user message has `content` parts; a row spelled the other way silently yields no turn. */
function harnessMessage(id: number, itemType: string, item: unknown) {
  return {
    id, worker_session_id: 'r', card_id: ASSISTANT_CARD.id, track_id: 'w1', thread_id: 't',
    turn_id: null, item_uuid: null, item_type: itemType, method: 'item/completed',
    params: JSON.stringify({ item, completedAtMs: id }), created_at_ms: id,
  };
}

/** The row the kernel writes for a drained user message BEFORE codex echoes it: a completed `userMessage` with no turn yet, keyed by the queue entry id. */
function projectionRow(id: number, clientId: string, text: string) {
  return {
    ...harnessMessage(id, 'userMessage', {
      id: clientId, clientId, type: 'userMessage', content: [{ type: 'text', text: `User says:\n${text}` }],
    }),
    item_uuid: clientId,
    params: JSON.stringify({
      item: { id: clientId, clientId, type: 'userMessage', content: [{ type: 'text', text: `User says:\n${text}` }] },
      _projection: true,
    }),
    input_segments: [{ presentation: 'user', text: `User says:\n${text}`, attachments: [] }],
  };
}

/** The same row after codex's completed echo upgraded it in place: same `id`,
 *  now with a turn and codex's own item id; segments untouched. */
function upgradedRow(row: ReturnType<typeof projectionRow>, turnId: string, itemUuid: string) {
  const params = JSON.parse(row.params) as { item: { content: unknown } };
  return {
    ...row, turn_id: turnId, item_uuid: itemUuid,
    params: JSON.stringify({ completedAtMs: row.id, item: { id: itemUuid, clientId: row.item_uuid, type: 'userMessage', content: params.item.content } }),
  };
}

/** The card a `/api/cards/{id}/…` request is about. */
function pathCardId(path: string): string {
  return decodeURIComponent(path.split('/')[3] ?? '');
}

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

/* The shapes are schema-checked by the transport; an off-schema body is refused before any test can observe anything. */
const inputAccepted = () => ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r' });
const runIdle = () => ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null });

function created(body: unknown): ApiTransportResponse {
  return { status: 201, statusText: 'Created', body };
}

function failure(status: number, code: string, error: string): ApiTransportResponse {
  return { status, statusText: 'Error', body: { code, error } };
}

type Reply = (request: ApiRequest) => ApiTransportResponse | undefined
  | Promise<ApiTransportResponse | undefined>;

function setup(reply?: Reply, storage?: UiPreferenceStorage, recovery?: RecoveryAccess) {
  const requests: ApiRequest[] = [];
  const themeValues = new Map<string, string>();
  const themeStorage: Pick<Storage, 'getItem' | 'setItem'> = {
    getItem: (key) => themeValues.get(key) ?? null,
    setItem: (key, value) => { themeValues.set(key, value); },
  };
  const transport: ApiTransportPort = {
    async send(request) {
      requests.push(request);
      if (reply) {
        const response = await reply(request);
        if (response) return response;
      }
      if (request.path === '/api/areas') return ok([AREA]);
      if (request.path === '/api/areas/c1/tracks') return ok([TRACK, BARE_TRACK]);
      if (request.path === '/api/overlays?entity_kind=track') return ok([]);
      if (request.path === '/api/tracks/w1') {
        return ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD], overlays: [],
        });
      }
      if (request.path === '/api/tracks/w2') return ok({
        track: BARE_TRACK, can_resume: false, cards: [], overlays: [],
      });
      if (request.path === CONVERSATIONS) return ok([assistantRow()]);
      if (request.path === BARE_CONVERSATIONS) return ok([]);
      if (request.path.includes(HISTORY_PATH)) return ok([]);
      /* Both card endpoints echo the card in the path, as the kernel does; a fixed id
         would be a trap for the first case that reads the field. */
      if (request.path.endsWith('/planner/run')) return ok({ card_id: pathCardId(request.path), worker_session_id: 'r', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null });
      /* An off-schema body is refused by the transport and the optimistic echo rolled back. */
      if (request.path.endsWith('/planner/input')) return ok({ card_id: pathCardId(request.path), worker_session_id: 'r' });
      if (request.path === '/api/settings') return ok({});
      return ok([]);
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
  const router = createAppRouter({ transport: recovery ? createRecoveryTransports(transport, recovery).business : transport, unauthorized, client, onSignOut: vi.fn(), cards: bootTestCardRuntime(), uiPreferences: createUiPreferences(storage) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={themeStorage}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { client, requests, router };
}

function cachedHistoryKey(client: QueryClient, cardId: string): readonly unknown[] {
  const key = client.getQueryCache().getAll().find((query) => {
    const data = query.state.data;
    return query.queryKey[1] === cardId
      && typeof data === 'object' && data !== null && 'pages' in data;
  })?.queryKey;
  if (key === undefined) throw new Error('history query was not cached');
  return key;
}

const creates = (requests: readonly ApiRequest[], path: string) =>
  requests.filter((request) => request.method === 'POST' && request.path === path);

/* The row the POST would mint: the panel looks for the id derived from
 * `(trackId, key)`, not for "a row that was not there before". */
const derivedRow = (trackId: string, request: ApiRequest): Row => ({
  id: trackConversationCardId(trackId, request.headers?.['Idempotency-Key'] ?? ''),
  trackId, title: null, kind: 'track-assistant', state: null, updatedAt: 99,
  lastTurnCompletedAt: null,
});

/* The open drawer, reached by the control only it has: an adopted assistant row
   is named `Assistant`, so the name cannot stand in. */
function drawerElement(): HTMLElement {
  const closer = screen.getByRole('button', { name: 'Close conversation' });
  const drawer = closer.closest('[role="complementary"]');
  if (drawer === null) throw new Error('the drawer is not open');
  return drawer as HTMLElement;
}

/* Scoped to the drawer: the list row behind it carries the same marker. */
const drawerWorkingMark = () => drawerElement().querySelector('[data-nc-activity="working"]');

async function openDraft() {
  fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
  /* The `+` is "New conversation"; the drawer it opens is "Untitled". */
  await screen.findByRole('complementary', { name: 'Untitled' });
}

function messageField(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

/* The composer is Astryx's contenteditable div: no value setter, so `change`
   throws, and it sends on a bare Enter. */
async function typeInto(field: HTMLElement, text: string) {
  field.textContent = text;
  const range = document.createRange();
  range.setStart(field.firstChild!, text.length);
  range.collapse(true);
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
  await act(async () => {
    fireEvent.input(field);
    await Promise.resolve();
  });
}

async function write(text: string) {
  const field = messageField();
  await typeInto(field, text);
  await submit();
}

/** Send by Enter, the only door while a turn runs: Astryx's `handleSubmit` never consulted `isStopShown`. */
async function submit() {
  await act(async () => {
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    await Promise.resolve();
  });
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/track/w1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('track conversations', () => {
  it('restores each track conversation drawer after switching tracks', async () => {
    const { router } = setup();
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Planner chat' }));
    await screen.findByRole('complementary', { name: 'Planner chat' });
    await act(() => router.navigate({ to: '/track/w2' }));
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();
    await act(() => router.navigate({ to: '/track/w1' }));
    await screen.findByRole('complementary', { name: 'Planner chat' });
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    await act(() => router.navigate({ to: '/track/w2' }));
    await act(() => router.navigate({ to: '/track/w1' }));
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();
  });

  it('lists the track\'s assistant conversations beside the planner one', async () => {
    setup();
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await screen.findByRole('button', { name: 'Conversation Assistant' });
  });

  it('opens the planner conversation from the track input request', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackActivityOverlay({ attention: 'input', items: [cardInputItem(PLANNER_CARD.id, 'input', 4)],
            cards: [{ card_id: PLANNER_CARD.id, state: 'input' }] })],
        })
      : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Review Planner notification: Requires input to continue.' }));
    expect(await screen.findByRole('complementary', { name: 'Planner chat' })).toBeTruthy();
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Message' })).toBe(document.activeElement));
    expect(screen.getByRole('region', { name: 'Notifications' })
      .getAttribute('data-nc-notification-mode')).toBe('compact');
    expect(screen.getByRole('region', { name: 'Notifications' }).querySelector('strong')).toBeNull();
  });

  it('opens the requesting worker card instead of the Planner conversation', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackActivityOverlay({ attention: 'input', items: [cardInputItem(WORKER_CARD.id, 'input', 4)],
            cards: [{ card_id: WORKER_CARD.id, state: 'input' }] })],
        })
      : undefined);

    fireEvent.click(await screen.findByRole('button', { name: 'Review Worker notification: Requires input to continue.' }));
    await waitFor(() => expect(window.location.search).toContain('card=card-worker'));
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-worker"]')).toBeTruthy();
  });

  it('opens an Assistant input notification in its conversation instead of treating it as a worker card', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackActivityOverlay({ attention: 'input', items: [cardInputItem(ASSISTANT_CARD.id, 'input', 4)],
            cards: [{ card_id: ASSISTANT_CARD.id, state: 'input' }] })],
        })
      : undefined);

    fireEvent.click(await screen.findByRole('button', { name: 'Review Assistant notification: Requires input to continue.' }));
    expect(await screen.findByRole('complementary', { name: 'Assistant' })).toBeTruthy();
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Message' })).toBe(document.activeElement));
    expect(window.location.search).not.toContain('card=');
  });

  it('lists simultaneous Planner and Worker requests with a truthful count', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackActivityOverlay({ attention: 'failed',
            items: [cardInputItem(PLANNER_CARD.id, 'input', 4), cardInputItem(WORKER_CARD.id, 'failed', 5)],
            cards: [{ card_id: PLANNER_CARD.id, state: 'input' }, { card_id: WORKER_CARD.id, state: 'failed' }] })],
        })
      : undefined);

    const notice = await screen.findByRole('region', { name: 'Notifications' });
    expect(within(notice).getByText('2 items need attention')).toBeTruthy();
    expect(within(notice).getByText('Planner')).toBeTruthy();
    expect(within(notice).getByText('Worker')).toBeTruthy();
    expect(within(notice).getByText('Stopped with an error and needs attention.')).toBeTruthy();
    /* Newest first (`at_ms` desc): the Worker's later failure above the Planner's request. */
    expect(within(notice).getAllByRole('listitem').map((item) => item.getAttribute('data-nc-notification-state')))
      .toEqual(['errored', 'awaiting-input']);
    fireEvent.click(within(notice).getByRole('button', { name: 'Collapse notifications' }));
    expect(await screen.findByRole('button', { name: 'Open 2 notifications' })).toBeTruthy();
  });

  /* The aside is the overlay's `items` folded per card (`foldAttentionByCard`): a card
   * carrying both a task and a session item is one row, spoken for by its later item.
   * Card-less items stay one row each; a task with no worker card reviews to the track itself. */
  it('notifications sidebar folds a card\'s items into one row and keeps every card-less item', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackActivityOverlay({ attention: 'failed', items: [
            { kind: 'failed', source: 'task', id: 'impl', card_id: WORKER_CARD.id, at_ms: 7 },
            { kind: 'failed', source: 'session', id: 'ws-worker', card_id: WORKER_CARD.id, at_ms: 6 },
            { kind: 'failed', source: 'task', id: 'gate', card_id: null, at_ms: 5 },
            { kind: 'input', source: 'lifecycle', id: 'w1', card_id: null, at_ms: 4 },
          ], cards: [{ card_id: WORKER_CARD.id, state: 'failed' }] })],
        })
      : undefined);

    const notice = await screen.findByRole('region', { name: 'Notifications' });
    expect(within(notice).getByText('3 items need attention')).toBeTruthy();
    const items = within(notice).getAllByRole('listitem');
    expect(items).toHaveLength(3);
    expect(items.map((item) => item.textContent)).toEqual([
      'WorkerThe task failed and needs attention.Review',
      'Task gateThe task failed and needs attention.Review',
      'TrackThe track is waiting on you.Review',
    ]);
    expect(items.map((item) => item.getAttribute('data-nc-notification-state'))).toEqual(['errored', 'errored', 'awaiting-input']);
    /* One Worker row: the later (task, at_ms 7) item speaks for the card. */
    expect(within(notice).getAllByRole('button', { name: /^Review Worker notification/ })
      .map((button) => button.getAttribute('aria-label'))).toEqual([
      'Review Worker notification: The task failed and needs attention.',
    ]);
    fireEvent.click(within(notice).getByRole('button', { name: 'Review Task gate notification: The task failed and needs attention.' }));
    expect(await screen.findByRole('complementary', { name: 'Planner chat' })).toBeTruthy();
    expect(window.location.search).not.toContain('card=');
  });

  /* The twin of the fold case: the fold is per card, so two failed worker cards are two rows. */
  it('notifications sidebar keeps one row per failed card', async () => {
    const SECOND_WORKER = { ...WORKER_CARD, id: 'card-worker-2', title: 'Worker two', sort: 4 };
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD, SECOND_WORKER],
          overlays: [trackActivityOverlay({ attention: 'failed', items: [
            { kind: 'failed', source: 'task', id: 'impl', card_id: WORKER_CARD.id, at_ms: 7 },
            { kind: 'failed', source: 'task', id: 'gate', card_id: SECOND_WORKER.id, at_ms: 6 },
          ], cards: [{ card_id: WORKER_CARD.id, state: 'failed' }, { card_id: SECOND_WORKER.id, state: 'failed' }] })],
        })
      : undefined);

    const notice = await screen.findByRole('region', { name: 'Notifications' });
    expect(within(notice).getByText('2 items need attention')).toBeTruthy();
    expect(within(notice).getAllByRole('listitem').map((item) => item.textContent)).toEqual([
      'WorkerThe task failed and needs attention.Review',
      'Worker twoThe task failed and needs attention.Review',
    ]);
  });

  /* A worker card's row is named by the first line of its `payload.goal` (what the task is
   * about), never by the card title (the task key); a card without a goal keeps its label. */
  it('notifications sidebar names a worker card by the first line of its goal and a terminal card by its title', async () => {
    const GOAL_WORKER = {
      ...WORKER_CARD, id: 'card-goal', kind: 'claude', title: 'review-r6-a',
      payload: { goal: 'Review the parser split against the design\nThen post the verdict.', idempotency_key: 'w1:review-r6-a' },
    };
    const TERMINAL_CARD = { ...WORKER_CARD, id: 'card-term', kind: 'terminal', title: 'zsh', payload: { command: 'zsh' }, sort: 5 };
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, GOAL_WORKER, TERMINAL_CARD],
          overlays: [trackActivityOverlay({ attention: 'failed', items: [
            { kind: 'failed', source: 'task', id: 'review-r6-a', card_id: GOAL_WORKER.id, at_ms: 7 },
            { kind: 'failed', source: 'session', id: 'ws-term', card_id: TERMINAL_CARD.id, at_ms: 6 },
          ], cards: [{ card_id: GOAL_WORKER.id, state: 'failed' }, { card_id: TERMINAL_CARD.id, state: 'failed' }] })],
        })
      : undefined);

    const notice = await screen.findByRole('region', { name: 'Notifications' });
    expect(within(notice).getAllByRole('listitem').map((item) => item.textContent)).toEqual([
      'Review the parser split against the designThe task failed and needs attention.Review',
      'zshIts session failed and needs attention.Review',
    ]);
    expect(within(notice).queryByText('review-r6-a')).toBeNull();
  });

  it('ignores a retired kernel/card/status row and a plugin-authored activity row', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [
            cardStatusOverlay(WORKER_CARD.id, 'AwaitingInput', 5),
            { ...trackActivityOverlay({ attention: 'input', items: [cardInputItem(PLANNER_CARD.id, 'input', 4)] }),
              plugin_id: 'third-party' },
          ],
        })
      : undefined);

    await screen.findByRole('button', { name: 'Rename track' });
    expect(screen.queryByRole('region', { name: 'Notifications' })).toBeNull();
  });

  it('keeps the name derived from confirmed turns after the drawer closes', async () => {
    let historyAvailable = true;
    const { client } = setup((request) => request.path.includes(HISTORY_PATH)
      ? historyAvailable
        ? ok([harnessMessage(1, 'userMessage', { content: [{ text: 'Named from history' }] })])
        : new Promise(() => undefined)
      : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    expect(await screen.findByRole('complementary', { name: 'Named from history' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    const remembered = await screen.findByRole('button', { name: /Conversation Named from history/ });

    historyAvailable = false;
    client.removeQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) });
    fireEvent.click(remembered);
    expect(await screen.findByRole('complementary', { name: 'Named from history' })).toBeTruthy();
  });

  it('does not reconcile an identical follow-up against an older pending history read', async () => {
    let historyReads = 0;
    let holdReopen = false;
    let releaseOld!: (response: ApiTransportResponse) => void;
    const oldRead = new Promise<ApiTransportResponse>((resolve) => { releaseOld = resolve; });
    const first = harnessMessage(1, 'userMessage', { content: [{ text: 'repeat me' }] });
    const { client, requests } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) {
        historyReads += 1;
        if (historyReads === 1) return oldRead;
        if (holdReopen) return new Promise(() => undefined);
        /* The pre-send baseline and the immediate post-send refresh may both
           still contain only the first, identical message. */
        return ok([first]);
      }
      if (request.path.endsWith('/planner/input')) return inputAccepted();
      if (request.path.endsWith('/planner/run')) return runIdle();
      return undefined;
    });

    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('complementary', { name: 'Assistant' });
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(screen.getByText('Loading conversation…')).toBeTruthy();
    expect(requests.some((request) => request.path.endsWith('/planner/input'))).toBe(false);

    await act(async () => { releaseOld(ok([first])); await oldRead; });
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('repeat me');
    await waitFor(() => expect(requests.some((request) => request.path.endsWith('/planner/input'))).toBe(true));

    await waitFor(() => expect(historyReads).toBe(2));
    const drawer = await screen.findByRole('complementary', { name: 'repeat me' });
    await waitFor(() => expect(within(drawer).getAllByText('repeat me')).toHaveLength(2));

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    holdReopen = true;
    client.removeQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation repeat me/ }));
    const reopened = await screen.findByRole('complementary', { name: 'repeat me' });
    expect(within(reopened).getAllByText('repeat me')).toHaveLength(2);
  });

  it('keeps history failures out of the send channel and offers a retry', async () => {
    let reads = 0;
    const first = harnessMessage(1, 'userMessage', { content: [{ text: 'first message' }] });
    const { requests } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) {
        reads += 1;
        return reads === 1
          ? { status: 503, statusText: 'Service Unavailable', body: { error: 'history unavailable' } }
          : ok([first]);
      }
      return undefined;
    });

    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    expect((await screen.findByRole('alert')).textContent).toContain('history unavailable');
    expect(screen.queryByText(/Nothing said yet/)).toBeNull();
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(requests.some((request) => request.path.endsWith('/planner/input'))).toBe(false);

    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(reads).toBe(2);
  });

  it('hands a send failure to the same conversation after its drawer remounts', async () => {
    let release!: () => void;
    const held = new Promise<void>((resolve) => { release = resolve; });
    const { requests } = setup(async (request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      await held;
      return { status: 503, statusText: 'Service Unavailable', body: { error: 'send failed after remount' } };
    });

    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('keep this failure visible');
    await waitFor(() => expect(requests.some((request) => request.path.endsWith('/planner/input'))).toBe(true));

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: /Conversation Assistant/ }));
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    await act(async () => { release(); await held; });
    expect((await screen.findByRole('alert')).textContent).toContain('send failed after remount');

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Planner chat' }));
    expect(screen.queryByText('send failed after remount')).toBeNull();
  });

  /* `planner_harness_runtime_superseded` means nothing was stored and the same text
   * reaches the successor; the draft is restored, and the retry proves it is usable. */
  it('keeps the message in the composer when the runtime was superseded, and re-sends it', async () => {
    let refuse = true;
    const { requests } = setup((request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      if (refuse) {
        refuse = false;
        return {
          status: 409,
          statusText: 'Conflict',
          body: {
            error: 'this runtime is no longer the card\'s; your message was not stored — send it again',
            code: 'planner_harness_runtime_superseded',
          },
        };
      }
      return ok({ card_id: pathCardId(request.path), worker_session_id: 'r' });
    });

    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('reconcile the ledger');

    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeNull());
    await waitFor(() => expect(messageField().textContent).toBe('reconcile the ledger'));

    const before = requests.filter((request) => request.path.endsWith('/planner/input')).length;
    await act(async () => {
      fireEvent.keyDown(messageField(), { key: 'Enter' });
      await Promise.resolve();
    });
    await waitFor(() =>
      expect(requests.filter((request) => request.path.endsWith('/planner/input')).length)
        .toBe(before + 1));
    await waitFor(() => expect(messageField().textContent).toBe(''));
  });

  /* `planner_harness_dormant` is answered before anything is written, so the
   * sentence is unspent and must not leave the field. */
  it('keeps the message in the composer when the harness is dormant', async () => {
    setup((request) => request.path.endsWith('/planner/input')
      ? {
        status: 409,
        statusText: 'Conflict',
        body: {
          error: 'no recoverable planner harness session for this card; reset to start a session',
          code: 'planner_harness_dormant',
        },
      }
      : undefined);

    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('the sentence a dormant harness must not eat');

    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeNull());
    await waitFor(() =>
      expect(messageField().textContent).toBe('the sentence a dormant harness must not eat'));
  });

  /* `POST /planner/input` carries no `Idempotency-Key`, so a 503 cannot say whether
   * the text was stored; only a refusal the server names licenses the restore. */
  it('leaves the field empty when the send failed without saying the text was refused', async () => {
    setup((request) => request.path.endsWith('/planner/input')
      ? { status: 503, statusText: 'Service Unavailable', body: { code: 'unavailable', error: 'busy' } }
      : undefined);

    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('the ledger again');

    /* The premise: the failure really was reported, so an empty field is not
       an unanswered request being read as a decision. */
    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeNull());
    expect(messageField().textContent).toBe('');
  });

  /* The outcome the composer reads must obey `stillActive()` like every other effect
   * of a failure, or the first conversation's sentence lands in the second's composer. */
  it('does not put a refused sentence into the conversation the reader walked to', async () => {
    const held = new Map<string, () => void>();
    setup(async (request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      const cardId = pathCardId(request.path);
      await new Promise<void>((resolve) => { held.set(cardId, resolve); });
      return {
        status: 409,
        statusText: 'Conflict',
        body: {
          error: 'this runtime is no longer the card\'s; your message was not stored — send it again',
          code: 'planner_harness_runtime_superseded',
        },
      };
    });

    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('complementary', { name: 'Assistant' });
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('sentence meant for assistant');
    await waitFor(() => expect(held.has(ASSISTANT_CARD.id)).toBe(true));

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Planner chat' }));
    await screen.findByRole('complementary', { name: 'Planner chat' });
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));

    await act(async () => { held.get(ASSISTANT_CARD.id)?.(); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });

    /* The premise: the refusal really was delivered to this store, which is
       what the error line being absent must not be allowed to stand for. */
    expect(held.has(ASSISTANT_CARD.id)).toBe(true);
    expect(messageField().textContent).toBe('');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('[F4] retains a typed refusal after reopening without duplicating the mounted restore', async () => {
    let refuse = true;
    setup((request) => request.path.endsWith('/planner/input') && refuse
      ? failure(409, 'planner_harness_runtime_superseded', 'Your message was not stored') : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('Original typed refusal');
    await waitFor(() => expect(messageField().textContent).toBe('Original typed refusal'));
    expect(within(drawerElement()).getAllByText('Original typed refusal')).toHaveLength(1);
    await typeInto(messageField(), 'A newer unsent draft');
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
    expect(messageField().textContent).toBe('A newer unsent draft');
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    expect(within(drawerElement()).getByText('Original typed refusal')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    expect(messageField().textContent).toBe('Original typed refusal');
    refuse = false;
    await act(async () => { fireEvent.keyDown(messageField(), { key: 'Enter' }); await Promise.resolve(); });
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    expect(messageField().textContent).toBe('');
  });

  it('[F4] retains a rejected message across reopen and retries its exact text once', async () => {
    let attempts = 0;
    const text = 'Keep this rejected sentence';
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/input')) {
        attempts += 1;
        return attempts === 1 ? failure(429, 'rate_limited', 'Wait a moment') : inputAccepted();
      }
      if (request.path.includes(HISTORY_PATH)) return ok(attempts > 1
        ? [harnessMessage(1, 'userMessage', { content: [{ text }] })] : []);
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    expect((await screen.findByRole('alert')).textContent).toContain('Wait a moment');
    expect(within(drawerElement()).getByText(text)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    expect(within(drawerElement()).getByText(text)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(requests.filter((request) => request.path.endsWith('/planner/input')).map((request) => request.body))
      .toEqual([{ text }, { text }]);
    expect(within(drawerElement()).getAllByText(text)).toHaveLength(1);
  });

  it.each(['rejected', 'unknown'] as const)(
    '[F4] never labels the %s working-turn submission as queued, including after reopen', async (outcome) => {
      const text = `Keep ${outcome} queued attempt`;
      const { requests } = setup((request) => {
        if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null });
        if (request.path.endsWith('/planner/input')) {
          if (outcome === 'unknown') throw new Error('response dropped');
          return failure(429, 'rate_limited', 'Wait a moment');
        }
        return undefined;
      });
      fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
      await screen.findByRole('button', { name: 'Stop' });
      await typeInto(messageField(), text);
      await submit();
      await screen.findByRole('alert');
      expect(within(drawerElement()).getByText(text)).toBeTruthy();
      expect(document.querySelector('[data-nc-queued]')).toBeNull();
      expect(document.querySelector('[data-nc-queued-note]')).toBeNull();
      expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
      fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
      fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
      expect(within(drawerElement()).getByText(text)).toBeTruthy();
      expect(document.querySelector('[data-nc-queued-note]')).toBeNull();
    },
  );

  it('[F4] marks a working-turn message queued only after its POST is acknowledged', async () => {
    let resolve!: (response: ApiTransportResponse) => void;
    const held = new Promise<ApiTransportResponse>((answer) => { resolve = answer; });
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null });
      if (request.path.endsWith('/planner/input')) return held;
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('button', { name: 'Stop' });
    await typeInto(messageField(), 'Waiting for the server');
    await submit();
    await waitFor(() => expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1));
    expect(within(drawerElement()).getByText('Waiting for the server')).toBeTruthy();
    expect(document.querySelector('[data-nc-queued-note]')).toBeNull();
    await act(async () => { resolve(inputAccepted()); await held; });
    await screen.findByText('Queued · sends when this turn ends');
    expect(messageField().getAttribute('contenteditable')).toBe('true');
  });

  it('[F5] keeps an uncertain attempt distinct from an acknowledged queued echo and matching history', async () => {
    const text = 'Repeat after queue';
    let attempts = 0;
    let rows: ReturnType<typeof harnessMessage>[] = [];
    const { client, requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null });
      if (request.path.includes(HISTORY_PATH)) return ok(rows);
      if (request.path.endsWith('/planner/input')) {
        attempts += 1;
        if (attempts === 2) throw new Error('response dropped');
        return inputAccepted();
      }
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('button', { name: 'Stop' });
    for (let attempt = 1; attempt <= 2; attempt += 1) {
      await typeInto(messageField(), text);
      await submit();
      await waitFor(() => expect(attempts).toBe(attempt));
      if (attempt === 1) await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    }
    await screen.findByRole('alert');
    expect(within(drawerElement()).getAllByText(text)).toHaveLength(2);
    expect(document.querySelectorAll('[data-nc-queued]')).toHaveLength(1);
    rows = [harnessMessage(1, 'userMessage', { content: [{ text }] })];
    await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
    await screen.findByText('A matching message is visible. Delivery is still unconfirmed.');
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(document.querySelectorAll('[data-nc-queued]')).toHaveLength(0);
    fireEvent.click(screen.getByRole('button', { name: 'I’ve checked' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(2);
  });

  it('[F4] edits a rejected message without replaying it before an explicit send', async () => {
    const { requests } = setup((request) => request.path.endsWith('/planner/input')
      ? failure(429, 'rate_limited', 'Wait a moment') : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('Original rejected message');
    await screen.findByRole('alert');
    fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    expect(messageField().textContent).toBe('Original rejected message');
    expect(messageField().getAttribute('contenteditable')).toBe('true');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
    await write('Revised request');
    await screen.findByRole('alert');
    expect(requests.filter((request) => request.path.endsWith('/planner/input')).map((request) => request.body))
      .toEqual([{ text: 'Original rejected message' }, { text: 'Revised request' }]);
  });

  it.each(['transport', '503', 'decode'] as const)(
    '[F5] replaces the %s failure with matching-message review until acknowledged', async (mode) => {
      const text = 'repeat the same request';
      const first = harnessMessage(1, 'userMessage', { content: [{ text }] });
      let rows = [first];
      const { client, requests } = setup((request) => {
        if (request.path.includes(HISTORY_PATH)) return ok(rows);
        if (request.path.endsWith('/planner/input')) {
          if (mode === 'transport') throw new Error('response dropped');
          return mode === '503' ? failure(503, 'unavailable', 'upstream unavailable') : ok({});
        }
        return undefined;
      });
      fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
      await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
      await write(text);
      await screen.findByRole('alert');
      expect(within(drawerElement()).getAllByText(text)).toHaveLength(2);
      expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
      fireEvent.click(screen.getByRole('button', { name: 'Check delivery' }));
      await waitFor(() => expect(requests.filter((request) => request.path.includes(HISTORY_PATH)).length).toBeGreaterThan(1));
      expect(screen.getByRole('alert').textContent).toContain('Delivery is unconfirmed');
      rows = [first, harnessMessage(2, 'userMessage', { content: [{ text: `${text}\nwith different instructions` }] })];
      await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
      expect(screen.getByRole('alert').textContent).toContain('Delivery is unconfirmed');

      expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
      fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
      fireEvent.click(await screen.findByRole('button', { name: /Conversation repeat the same request/ }));
      expect(within(drawerElement()).getAllByText(text)).toHaveLength(2);
      rows = [first, harnessMessage(3, 'userMessage', { content: [{ text }] }),
        harnessMessage(4, 'agentMessage', { text: 'Received once' })];
      await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
      await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
      expect((await screen.findByText('A matching message is visible. Delivery is still unconfirmed.')).closest('[role="status"]')?.textContent).toContain('Delivery is still unconfirmed');
      expect(within(drawerElement()).getAllByText(text)).toHaveLength(2);
      expect(screen.getByText('Received once')).toBeTruthy();
      expect(messageField().getAttribute('contenteditable')).toBe('false');
      expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
      fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
      fireEvent.click(await screen.findByRole('button', { name: /Conversation repeat the same request/ }));
      expect((await screen.findByText('A matching message is visible. Delivery is still unconfirmed.')).closest('[role="status"]')?.textContent).toContain('Delivery is still unconfirmed');
      fireEvent.click(screen.getByRole('button', { name: 'I’ve checked' }));
      await waitFor(() => expect(screen.queryByText(/A matching message is visible/)).toBeNull());
      expect(messageField().getAttribute('contenteditable')).toBe('true');
      expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
    },
  );

  it('[F4] requires an explicit duplicate-risk confirmation before an ambiguous resend', async () => {
    let attempts = 0;
    const text = 'Keep the dropped request';
    const { requests } = setup((request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      attempts += 1;
      if (attempts === 1) throw new Error('response dropped');
      return inputAccepted();
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    await screen.findByRole('alert');
    fireEvent.click(screen.getByRole('button', { name: 'Send again…' }));
    const dialog = await screen.findByRole('dialog', { name: 'Send this message again?' });
    expect(within(dialog).getByText(/may already have arrived/)).toBeTruthy();
    expect(attempts).toBe(1);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(attempts).toBe(1);
    expect(within(drawerElement()).getByText(text)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Send again…' }));
    fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Send again' }));
    await waitFor(() => expect(attempts).toBe(2));
    expect(requests.filter((request) => request.path.endsWith('/planner/input')).map((request) => request.body))
      .toEqual([{ text }, { text }]);
  });

  /* A retry must carry the images the failed message was shown with; the defect
   * was in what the retry put on the wire. */
  const ATTACHMENT_ID = '0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png';

  function withAttachments(onInput: (attempt: number) => ApiTransportResponse | undefined) {
    let attempts = 0;
    return setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({
          card_id: pathCardId(request.path), worker_session_id: 'r', phase: 'idle',
          attachments_supported: true,
        });
      }
      if (request.path.endsWith('/planner/attachments')) {
        return ok({
          attachmentId: ATTACHMENT_ID, contentType: 'image/png', size: 4,
          url: `/api/cards/${pathCardId(request.path)}/planner/attachments/${ATTACHMENT_ID}`,
        });
      }
      if (request.path.endsWith('/planner/input')) {
        attempts += 1;
        return onInput(attempts);
      }
      return undefined;
    });
  }

  async function attachAnImage() {
    /* The hidden input behind the attach button; the `IconButton` carries the accessible name. */
    const picker = document.querySelector<HTMLInputElement>('input[type="file"]');
    if (picker === null) throw new Error('no file input rendered');
    const file = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], 'shot.png', { type: 'image/png' });
    await act(async () => {
      fireEvent.change(picker, { target: { files: [file] } });
      await Promise.resolve();
    });
  }

  function inputBodies(requests: readonly ApiRequest[]) {
    return requests.filter((request) => request.path.endsWith('/planner/input'))
      .map((request) => request.body);
  }

  it('[#1505] re-sends the image the failed message was shown with, not just its words', async () => {
    const text = 'look at this';
    const { requests } = withAttachments((attempt) => attempt === 1
      ? ({ status: 400, statusText: 'Bad Request', body: { error: 'nope', code: 'bad_request' } })
      : ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await attachAnImage();
    await write(text);

    // The failure is shown WITH the thumbnail.
    await screen.findByRole('alert');
    expect(drawerElement().querySelector('[data-nc-turn-attachments] img')?.getAttribute('src'))
      .toBe(`/api/cards/${ASSISTANT_CARD.id}/planner/attachments/${ATTACHMENT_ID}`);

    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(2));
    expect(inputBodies(requests)).toEqual([
      { text, attachments: [ATTACHMENT_ID] },
      { text, attachments: [ATTACHMENT_ID] },
    ]);
  });

  it('[#1505] retrying an image-only message does not post the one body the server refuses', async () => {
    const { requests } = withAttachments((attempt) => attempt === 1
      ? ({ status: 400, statusText: 'Bad Request', body: { error: 'nope', code: 'bad_request' } })
      : ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await attachAnImage();

    // The vendor send button is unavailable on an empty draft; this is the control the composer grows for that case.
    const send = drawerElement().querySelector('[data-nc-send-attachment]');
    expect(send).toBeTruthy();
    await act(async () => {
      fireEvent.click(send as HTMLElement);
      await Promise.resolve();
    });
    await screen.findByRole('alert');

    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(2));
    /* `{ text: '' }` alone is the body `validate_planner_input` refuses, and
         `sendBlocked` stays true while a failure is outstanding. */
    expect(inputBodies(requests)[1]).toEqual({ text: '', attachments: [ATTACHMENT_ID] });
  });

  it('[F6] replaces stale Working with a stuck explanation and preserves the unsent draft', async () => {
    let phase = 'turn_running';
    const { client, requests } = setup((request) => {
      if (request.path === CONVERSATIONS) return ok([assistantRow({ state: 'turn_pending' })]);
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase, model: null, reasoning_effort: null, blocked_reason: null });
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await typeInto(messageField(), 'Draft written before the stall');
    phase = 'wedged';
    await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', ASSISTANT_CARD.id] }); });
    expect((await screen.findByRole('alert')).textContent).toContain('This conversation is stuck');
    expect(drawerWorkingMark()).toBeNull();
    expect(messageField().textContent).toBe('Draft written before the stall');
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(screen.queryByRole('button', { name: 'Stop' })).toBeNull();
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(messageField().textContent).toBe('Draft written before the stall');
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(0);
    fireEvent.click(screen.getByRole('button', { name: 'Start a new conversation' }));
    expect(await screen.findByRole('complementary', { name: 'Untitled' })).toBeTruthy();
    expect(messageField().textContent).toBe('Draft written before the stall');
    expect(messageField().getAttribute('contenteditable')).toBe('true');
  });

  it('[F6] stops promising queued delivery after the harness becomes wedged', async () => {
    let phase = 'turn_running';
    const { client, requests } = setup((request) => request.path.endsWith('/planner/run')
      ? ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase, model: null, reasoning_effort: null, blocked_reason: null }) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('button', { name: 'Stop' });
    await typeInto(messageField(), 'Queued before the stall');
    await submit();
    await screen.findByText('Queued · sends when this turn ends');
    phase = 'wedged';
    await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', ASSISTANT_CARD.id] }); });
    expect((await screen.findByRole('alert')).textContent).toContain('This conversation is stuck');
    expect(within(drawerElement()).getByText('Queued before the stall')).toBeTruthy();
    expect(document.querySelector('[data-nc-queued-note]')).toBeNull();
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
  });

  it('[F6] stops claiming Working when the runtime wedges during an unanswered send', async () => {
    let phase = 'idle';
    let release!: (value: ApiTransportResponse) => void;
    const held = new Promise<ApiTransportResponse>((resolve) => { release = resolve; });
    const { client } = setup((request) => {
      if (request.path.endsWith('/planner/input')) return held;
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase, model: null, reasoning_effort: null, blocked_reason: null });
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('Keep the pending message');
    expect(drawerWorkingMark()).not.toBeNull();
    phase = 'wedged';
    await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', ASSISTANT_CARD.id] }); });
    expect((await screen.findByRole('alert')).textContent).toContain('This conversation is stuck');
    expect(drawerWorkingMark()).toBeNull();
    expect(within(drawerElement()).getByText('Keep the pending message')).toBeTruthy();
    await act(async () => { release(inputAccepted()); await held; });
    expect(drawerWorkingMark()).toBeNull();
  });

  /* The POST is what is asserted, not the drawer: a `+` that opened a draft nothing
   * could be sent from would satisfy any assertion about the button. */
  it('[G4] starts a conversation on a track that has no planner card', async () => {
    /* Stateful on purpose: a real server lists the row it just minted, and the create
           invalidates this very list. */
    const minted: Row[] = [];
    const { requests, router } = setup((request) => {
      if (request.method === 'POST' && request.path === BARE_CONVERSATIONS) {
        const row = derivedRow('w2', request);
        minted.push(row);
        return created(row);
      }
      return request.path === BARE_CONVERSATIONS ? ok([...minted]) : undefined;
    });
    await act(async () => { await router.navigate({ to: '/track/w2' }); });
    await screen.findByText('No conversations yet.');
    await openDraft();
    /* Nothing is minted by opening the drawer; the card is minted by the first message. */
    expect(creates(requests, BARE_CONVERSATIONS)).toHaveLength(0);
    await write('what is in this repo?');
    await waitFor(() => expect(creates(requests, BARE_CONVERSATIONS)).toHaveLength(1));
    const [post] = creates(requests, BARE_CONVERSATIONS);
    expect(post?.body).toEqual({ text: 'what is in this repo?' });
    expect(post?.headers?.['Idempotency-Key']).toMatch(/[0-9a-f-]{36}/);
    /* The drawer moves off the draft onto the row the derived id names. */
    await screen.findByRole('complementary', { name: 'Assistant' });
  });

  it('[G4] sends the first message once, to the track in the URL and no other', async () => {
    const { requests } = setup((request) =>
      request.method === 'POST' && request.path === CONVERSATIONS
        ? created(derivedRow('w1', request))
        : undefined);
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('first words');
    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));
    expect(requests.filter((request) => request.method === 'POST'
      && request.path.endsWith('/conversations') && request.path !== CONVERSATIONS)).toEqual([]);
    /* The message travelled with the POST, so nothing re-sends it afterwards. */
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toEqual([]);
  });

  /* The server may have committed the first POST before its 500 reached us;
   * retrying under a fresh key would create a second conversation. Two tracks, so
   * a root-level slot would trade the remount bug for a scope-switch bug. */
  it('keeps each failed draft key across track route remounts', async () => {
    const attempts = new Map<string, number>();
    const { requests, router } = setup((request) => {
      if (request.method !== 'POST'
        || (request.path !== CONVERSATIONS && request.path !== BARE_CONVERSATIONS)) return undefined;
      const attempt = (attempts.get(request.path) ?? 0) + 1;
      attempts.set(request.path, attempt);
      if (attempt === 1) return failure(500, 'internal', 'boom');
      return created(derivedRow(request.path === CONVERSATIONS ? 'w1' : 'w2', request));
    });

    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('words for track one');
    await screen.findByRole('button', { name: 'Try again' });

    await act(async () => { await router.navigate({ to: '/track/w2' }); });
    await screen.findByText('No conversations yet.');
    await openDraft();
    await write('words for track two');
    await screen.findByRole('button', { name: 'Try again' });

    await act(async () => { await router.navigate({ to: '/track/w1' }); });
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    expect(screen.getByText('words for track one')).toBeTruthy();
    fireEvent.click(await screen.findByRole('button', { name: 'Try again' }));

    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(2));
    const [first, retry] = creates(requests, CONVERSATIONS);
    expect(first?.headers?.['Idempotency-Key']).toBeDefined();
    expect(retry?.headers?.['Idempotency-Key']).toBe(first?.headers?.['Idempotency-Key']);
    expect([first?.body, retry?.body]).toEqual([
      { text: 'words for track one' },
      { text: 'words for track one' },
    ]);

    await act(async () => { await router.navigate({ to: '/track/w2' }); });
    await screen.findByText('No conversations yet.');
    await openDraft();
    expect(screen.getByText('words for track two')).toBeTruthy();
    fireEvent.click(await screen.findByRole('button', { name: 'Try again' }));

    await waitFor(() => expect(creates(requests, BARE_CONVERSATIONS)).toHaveLength(2));
    const [bareFirst, bareRetry] = creates(requests, BARE_CONVERSATIONS);
    expect(bareFirst?.headers?.['Idempotency-Key']).toBeDefined();
    expect(bareRetry?.headers?.['Idempotency-Key'])
      .toBe(bareFirst?.headers?.['Idempotency-Key']);
  });

  /* If only `{ key, sentText }` survives a remount, the new route believes creation
   * is idle and lets both requests create a row. */
  it('keeps an in-flight draft locked across a route remount', async () => {
    let releaseFirst!: (response: ApiTransportResponse) => void;
    const firstCreate = new Promise<ApiTransportResponse>((resolve) => { releaseFirst = resolve; });
    let landed: Row | null = null;
    const { requests, router } = setup((request) => {
      if (request.path === CONVERSATIONS && request.method === 'GET' && landed !== null) {
        return ok([assistantRow(), landed]);
      }
      if (request.path !== CONVERSATIONS || request.method !== 'POST') return undefined;
      return creates(requests, CONVERSATIONS).length === 1
        ? firstCreate
        : created(derivedRow('w1', request));
    });

    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('words still in flight');
    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));

    await act(async () => { await router.navigate({ to: '/' }); });
    await act(async () => { await router.navigate({ to: '/track/w1' }); });
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();

    expect(screen.getByText('words still in flight')).toBeTruthy();
    expect(screen.getByText('Sending…')).toBeTruthy();
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    await write('edited while the first request is pending');
    expect(creates(requests, CONVERSATIONS)).toHaveLength(1);

    const first = creates(requests, CONVERSATIONS)[0];
    landed = derivedRow('w1', first);
    await act(async () => {
      releaseFirst(created(landed));
      await firstCreate;
    });
    await screen.findByRole('complementary', { name: 'Assistant' });
    expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
  });

  it('unlocks the fresh key after an exhausted attempt is rekeyed', async () => {
    const { requests } = setup((request) => {
      if (request.path !== CONVERSATIONS || request.method !== 'POST') return undefined;
      return creates(requests, CONVERSATIONS).length === 1
        ? failure(409, 'idempotency_key_exhausted', 'this key is used up')
        : created(derivedRow('w1', request));
    });

    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('retry after exhaustion');
    const retry = await screen.findByRole('button', { name: 'Try again' });
    expect(retry.hasAttribute('disabled')).toBe(false);
    fireEvent.click(retry);

    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(2));
    const [exhausted, fresh] = creates(requests, CONVERSATIONS);
    expect(fresh?.headers?.['Idempotency-Key'])
      .not.toBe(exhausted?.headers?.['Idempotency-Key']);
  });

  /* At most one echo is ever unanswered: clearing the send state unconditionally
   * on a conversation switch would re-open a composer whose own message is still
   * in flight. The second POST is what this asserts. */
  it('[G5] lets no stale request re-open a composer whose own message is still in flight', async () => {
    const held = new Map<string, () => void>();
    const release = (cardId: string) => held.get(cardId)?.();
    const { requests } = setup(async (request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      const cardId = pathCardId(request.path);
      await new Promise<void>((resolve) => { held.set(cardId, resolve); });
      return { status: 503, statusText: 'Service Unavailable', body: { code: 'unavailable', error: 'busy' } };
    });
    /* Conversation A, one message, request still out. */
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('complementary', { name: 'Assistant' });
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('the first conversation speaks');
    await waitFor(() => expect(held.has(ASSISTANT_CARD.id)).toBe(true));

    /* Conversation B, on the same panel instance: the walk that resets `sendingRef`. */
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Planner chat' }));
    await screen.findByRole('complementary', { name: 'Planner chat' });
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('the second conversation speaks');
    await waitFor(() => expect(held.has(PLANNER_CARD.id)).toBe(true));
    const sends = () => requests.filter((request) =>
      request.path === `/api/cards/${PLANNER_CARD.id}/planner/input`);
    expect(sends()).toHaveLength(1);

    /* A's request lands now, answering for a conversation nobody is looking at. */
    await act(async () => { release(ASSISTANT_CARD.id); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });

    /* B's message is still unanswered, so this third message does not go out. */
    await write('and a third the store must refuse');
    await act(async () => { await Promise.resolve(); });
    expect(sends()).toHaveLength(1);
    /* Nor did A's failure surface under B's composer. */
    expect(screen.queryByText(/busy|Could not send/)).toBeNull();
  });

  /* An assistant card is headless; `codex` is scanned first and would otherwise
   * claim the card and put an empty terminal in this panel. */
  it('keeps assistant cards out of the CARDS panel, listing only the worker', async () => {
    setup();
    /* `[data-nc-card-inventory]` is the CARDS module's own list. */
    const list = await waitFor(() => {
      const found = document.querySelector('[data-nc-card-inventory]');
      if (found === null) throw new Error('card inventory has not rendered');
      return found as HTMLElement;
    });
    const labels = within(list).getAllByRole('listitem').map((row) => row.textContent ?? '');
    /* Title then kernel kind; the assistant card is absent entirely. */
    expect(labels).toEqual(['Workercodex']);
  });

  /* The kernel writes the first sentence to the transcript when the queue drains;
   * codex's echo upgrades the same row (same `id`), so the line renders once. */
  it('shows the first sentence from the transcript row the kernel writes at drain, once, through the echo', async () => {
    const minted: Row[] = [];
    let persisted: Record<string, unknown>[] = [];
    const { client, requests } = setup((request) => {
      if (request.method === 'POST' && request.path === CONVERSATIONS) {
        const row = derivedRow('w1', request);
        minted.push(row);
        /* The kernel writes the projection row before answering codex; the first item
                   read after the 201 already has it. */
        persisted = [projectionRow(1, 'entry-0001', 'start this thread')];
        return created(row);
      }
      if (request.path === CONVERSATIONS) return ok([assistantRow(), ...minted]);
      if (request.path.includes(HISTORY_PATH)) return ok([...persisted]);
      if (request.path.endsWith('/planner/run')) return runIdle();
      return undefined;
    });
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('start this thread');
    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));
    /* The draft is gone: this is the mounted thread, not the composer's show-back,
           which carries the same attribute. */
    await waitFor(() => expect(screen.queryByRole('complementary', { name: 'Untitled' })).toBeNull());
    const drawer = drawerElement();
    await waitFor(() => expect(
      [...drawer.querySelectorAll('[data-nc-turn="you"]')].map((turn) => turn.textContent),
    ).toEqual(['start this thread']));
    expect(drawer.querySelector('[data-nc-thread-empty]')).toBeNull();
    const before = drawer.querySelector('[data-nc-turn="you"]');

    /* codex echoes: the kernel upgrades the row in place. */
    persisted = [upgradedRow(projectionRow(1, 'entry-0001', 'start this thread'), 'turn-1', 'item-codex-1')];
    await act(async () => {
      await client.invalidateQueries({ queryKey: cachedHistoryKey(client, minted[0].id) });
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await waitFor(() => expect(
      [...drawer.querySelectorAll('[data-nc-turn="you"]')].map((turn) => turn.textContent),
    ).toEqual(['start this thread']));
    /* Same DOM node: the line is keyed by the row's `id`, which the upgrade keeps. */
    expect(drawer.querySelector('[data-nc-turn="you"]')).toBe(before);
  });

  /* The create's message never drained, so the kernel wrote no row; the reader
   * types it again, and the one row that lands belongs to that send. */
  it('leaves the composer open when the first sentence is retyped and one row lands', async () => {
    const minted: Row[] = [];
    let persisted: ReturnType<typeof harnessMessage>[] = [];
    const { requests } = setup((request) => {
      if (request.method === 'POST' && request.path === CONVERSATIONS) {
        const row = derivedRow('w1', request);
        minted.push(row);
        return created(row);
      }
      if (request.path === CONVERSATIONS) return ok([assistantRow(), ...minted]);
      if (request.path.includes(HISTORY_PATH)) return ok([...persisted]);
      if (request.path.endsWith('/planner/input')) return inputAccepted();
      if (request.path.endsWith('/planner/run')) return runIdle();
      return undefined;
    });
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('hi');
    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));
    await waitFor(() => expect(screen.queryByRole('complementary', { name: 'Untitled' })).toBeNull());
    const drawer = drawerElement();
    expect(drawer.querySelectorAll('[data-nc-turn="you"]')).toHaveLength(0);

    /* Nothing came back, so they say it again. The composer allows it. */
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    persisted = [harnessMessage(1, 'userMessage', { content: [{ text: 'hi' }] })];
    await write('hi');
    await waitFor(() => expect(requests.some((request) => request.path.endsWith('/planner/input'))).toBe(true));

    /* The row retires the send's echo, and the composer comes back. */
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect([...drawer.querySelectorAll('[data-nc-turn="you"]')].map((turn) => turn.textContent))
      .toEqual(['hi']);
  });

  /* An unknown transcript is not an empty one. */
  it('does not paint the empty state while the first page is still loading', async () => {
    setup((request) => request.path.includes(HISTORY_PATH)
      ? new Promise(() => undefined)
      : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    const drawer = await screen.findByRole('complementary', { name: 'Assistant' });
    expect(within(drawer).getByText('Loading conversation…')).toBeTruthy();
    expect(drawer.querySelector('[data-nc-thread-empty]')).toBeNull();
  });

  it('keeps the draft and reports the reason when the create is refused', async () => {
    setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
      ? failure(400, 'invalid_request', 'That message was refused.')
      : undefined);
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('refused words');
    expect((await screen.findByRole('alert')).textContent).toContain('That message was refused.');
    expect(screen.getByRole('complementary', { name: 'Untitled' })).toBeTruthy();
  });
});

/* What may enter the session registry, driven through the store itself. The
 * drawer is modelled by `scope`: `useConversationPanel` computes it from the open
 * row, so closing the drawer is handing this store a null scope while it stays mounted. */
describe('registry write-through', () => {
  const SCOPE = {
    id: 'w1', provider: 'codex' as const, title: 'Test track', cardId: ASSISTANT_CARD.id, cardTitle: null,
    updatedAt: 30, kind: 'track-assistant' as const, state: 'idle' as const,
  };
  const ROWS: readonly Conversation[] = [{
    id: ASSISTANT_CARD.id, trackId: 'w1', trackTitle: 'Test track', title: null,
    kind: 'track-assistant', state: 'idle', updatedAt: 30,
  }];

  /** One mounted store over one registry. `rows` is the track's server list, so the batch remember runs for real. */
  function mountStore(transport: ApiTransportPort, rows: readonly Conversation[] = ROWS) {
    let latestSend: (text: string) => void = () => undefined;
    let known: readonly Conversation[] = [];
    let readTurns: (id: string) => readonly TranscriptEntry[] = () => [];

    function StoreProbe({ scope }: { scope: typeof SCOPE | null }) {
      const store = useConversationStore(transport, unauthorized, scope, {
        rows, rememberOn: 'w1',
      });
      const send = store.send;
      useEffect(() => { latestSend = (text) => { void send(ASSISTANT_CARD.id, text); }; });
      return null;
    }

    function RegistryProbe() {
      const registry = useConversationRegistry();
      useEffect(() => { known = registry.conversations; readTurns = registry.turnsOf; });
      return null;
    }

    const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
    const view = (scope: typeof SCOPE | null) => (
      <QueryClientProvider client={client}>
        <ConversationProvider>
          <RegistryProbe />
          <StoreProbe scope={scope} />
        </ConversationProvider>
      </QueryClientProvider>
    );
    const { rerender } = render(view(SCOPE));
    return {
      /* The macrotask is not padding: `mutations.send` resolves two invalidations after
             its POST, and an invalidation only refetches for committed observers. */
      send: async (text: string) => {
        await act(async () => { latestSend(text); await new Promise((resolve) => setTimeout(resolve, 0)); });
      },
      /** The drawer shuts; the store stays mounted, as it does in production. */
      closeDrawer: async () => { await act(async () => { rerender(view(null)); await Promise.resolve(); }); },
      settle: async () => {
        await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
      },
      /** A history read landing, written where a landed history lives. */
      deliverHistory: async (rows: readonly unknown[]) => {
        await act(async () => {
          client.setQueryData(
            cachedHistoryKey(client, ASSISTANT_CARD.id),
            { pages: [rows], pageParams: [0] },
          );
          await Promise.resolve();
        });
      },
      entry: () => known.find((candidate) => candidate.id === ASSISTANT_CARD.id),
      turns: () => readTurns(ASSISTANT_CARD.id),
    };
  }

  /* The server's row is `title: null` for the life of an assistant conversation;
   * the only name is the one the drawer derives, and the batch remember must carry it. */
  it('[G5] keeps the name it derived from the first message after the drawer closes', async () => {
    const transport: ApiTransportPort = {
      send(request) {
        if (request.path.endsWith('/planner/input')) return Promise.resolve(inputAccepted());
        if (request.path.endsWith('/planner/run')) return Promise.resolve(runIdle());
        return Promise.resolve(ok([]));
      },
    };
    const store = mountStore(transport);
    await store.send('rename this conversation');
    await waitFor(() => { expect(store.entry()?.title).toBe('rename this conversation'); });
    await store.closeDrawer();
    await store.settle();
    /* The batch remember did not put the server's `title: null` back over it. */
    expect(store.entry()?.title).toBe('rename this conversation');
  });

  /* The order is the test: close the drawer while the POST is in flight, so `scope`
   * is null when it fails and the `catch` that drops the echo reaches nothing.
   * Rejecting before the close is green with or without the fix. */
  it('[G5] does not keep a name, or a time, from a message that failed to send', async () => {
    let rejectInput!: () => void;
    const settled = new Promise<void>((resolve) => { rejectInput = resolve; });
    const transport: ApiTransportPort = {
      async send(request) {
        if (request.path.endsWith('/planner/input')) {
          await settled;
          return { status: 503, statusText: 'Service Unavailable', body: { code: 'unavailable', error: 'busy' } };
        }
        if (request.path.endsWith('/planner/run')) return runIdle();
        return ok([]);
      },
    };
    const store = mountStore(transport);
    const beforeSend = Date.now();
    await store.send('a message that never lands');
    /* Shut, with the POST still out: this is the window, and releasing the
       rejection before this point would test a different code path. */
    await store.closeDrawer();
    await act(async () => { rejectInput(); await Promise.resolve(); });
    await store.settle();

    /* No name: the kind label is all a `title` of null renders as. */
    expect(store.entry()?.title ?? null).toBeNull();
    /* No time from a clock only this browser read: `beforeSend` is the fence, an
           echo's `atMs` is `Date.now()`. */
    expect(store.entry()?.updatedAt).toBe(30);
    expect(store.entry()?.updatedAt).toBeLessThan(beforeSend);
    /* Nor is the message itself remembered as something that happened. */
    expect(store.turns()).toHaveLength(0);
  });

  /* The write-through may not undo what arrived while it was waiting: read and
   * write must be the same moment (`updateExisting`). Two turns rejects both a
   * captured snapshot (`1`) and an appending merge (`3`). */
  it('[G5] does not overwrite a refresh that landed while the send was still settling', async () => {
    let releaseInput!: () => void;
    const inputSettled = new Promise<void>((resolve) => { releaseInput = resolve; });
    const transport: ApiTransportPort = {
      async send(request) {
        if (request.path.endsWith('/planner/input')) {
          await inputSettled;
          return inputAccepted();
        }
        if (request.path.endsWith('/planner/run')) return runIdle();
        return ok([]);
      },
    };
    const store = mountStore(transport);
    await store.send('what does this repo do?');
    /* The history refresh lands while the POST that started it is still out. */
    await store.deliverHistory([
      harnessMessage(1, 'userMessage', { content: [{ text: 'what does this repo do?' }] }),
      harnessMessage(2, 'agentMessage', { text: 'it runs tracks' }),
    ]);
    await waitFor(() => { expect(store.turns()).toHaveLength(2); });

    await store.closeDrawer();
    await act(async () => { releaseInput(); await Promise.resolve(); });
    await store.settle();
    expect(store.turns()).toHaveLength(2);
    expect(store.entry()?.turns).toBe(2);
  });

  /* The "already brought back?" check matches by text, so asked against the whole
   * entry an old identical `ping` answers for the new one; only rows that arrived
   * since the send may answer. */
  it('[G5] counts a message really sent twice, when the refresh is a moment behind', async () => {
    let releaseInput!: () => void;
    const inputSettled = new Promise<void>((resolve) => { releaseInput = resolve; });
    const transport: ApiTransportPort = {
      async send(request) {
        /* The server's copy of the first `ping`, and only ever that one. */
        if (request.path.includes(HISTORY_PATH)) {
          return ok([harnessMessage(1, 'userMessage', { content: [{ text: 'ping' }] })]);
        }
        if (request.path.endsWith('/planner/input')) {
          await inputSettled;
          return inputAccepted();
        }
        if (request.path.endsWith('/planner/run')) return runIdle();
        return ok([]);
      },
    };
    const store = mountStore(transport);
    /* The first `ping` is already in the transcript. */
    await waitFor(() => { expect(store.turns()).toHaveLength(1); });
    await store.send('ping');

    await store.closeDrawer();
    await act(async () => { releaseInput(); await Promise.resolve(); });
    await store.settle();
    expect(store.turns()).toHaveLength(2);
    expect(store.entry()?.turns).toBe(2);
  });

  /* Two real store instances under one provider; the first request crosses the remount. */
  it('[G5] serializes same-card sends across a remount and keeps both identical turns', async () => {
    /* Every send is held, so both are still out when their stores unmount. */
    const holds: (() => void)[] = [];
    let historyRows: readonly unknown[] = [];
    const transport: ApiTransportPort = {
      async send(request) {
        if (request.path.endsWith('/planner/input')) {
          await new Promise<void>((resolve) => { holds.push(resolve); });
          return inputAccepted();
        }
        if (request.path.endsWith('/planner/run')) return runIdle();
        if (request.path.includes(HISTORY_PATH)) return ok(historyRows);
        return ok([]);
      },
    };
    let latestSend: (text: string) => void = () => undefined;
    let visibleTurns: readonly TranscriptEntry[] = [];
    let latestTurns: readonly TranscriptEntry[] = [];

    function StoreProbe() {
      const store = useConversationStore(transport, unauthorized, SCOPE, {
        rows: ROWS, rememberOn: 'w1',
      });
      const send = store.send;
      const turns = store.turnsOf(ASSISTANT_CARD.id);
      useEffect(() => {
        latestSend = (text) => { void send(ASSISTANT_CARD.id, text); };
        visibleTurns = turns;
      });
      return null;
    }

    function RegistryProbe() {
      const turns = useConversationRegistry().turnsOf(ASSISTANT_CARD.id);
      useEffect(() => { latestTurns = turns; });
      return null;
    }

    const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
    const view = (instance: string | null) => (
      <QueryClientProvider client={client}>
        <ConversationProvider>
          <RegistryProbe />
          {instance === null ? null : <StoreProbe key={instance} />}
        </ConversationProvider>
      </QueryClientProvider>
    );
    const { rerender } = render(view('first-mount'));

    await act(async () => { latestSend('ping'); await Promise.resolve(); });
    await waitFor(() => expect(holds).toHaveLength(1));
    /* The walk away: this store is gone, its request still out. */
    await act(async () => { rerender(view(null)); await Promise.resolve(); });
    /* The walk back: a new store, same conversation, same registry. */
    await act(async () => { rerender(view('second-mount')); await Promise.resolve(); });
    await act(async () => { latestSend('ping'); await Promise.resolve(); });
    /* The provider-wide per-card lease keeps a remount from starting another
       send while the first request is unresolved. */
    expect(holds).toHaveLength(1);

    /* The first POST lands while the second store is mounted; only after it settles
           may the new store start its same-text send. */
    historyRows = [harnessMessage(1, 'userMessage', { content: [{ text: 'ping' }] })];
    await act(async () => { holds[0]?.(); await Promise.resolve(); });
    await waitFor(() => {
      latestSend('ping');
      expect(holds).toHaveLength(2);
    });
    await waitFor(() => expect(visibleTurns).toHaveLength(2));

    /* The second answer lands after its store is gone, against the same stale
       one-row history. Its write-through must retain the second echo. */
    await act(async () => { rerender(view(null)); await Promise.resolve(); });
    await act(async () => {
      holds[1]?.();
      await Promise.resolve();
    });
    await waitFor(() => expect(latestTurns).toHaveLength(2));
    expect(latestTurns.map((turn) => 'text' in turn ? turn.text : '')).toEqual(['ping', 'ping']);
    /* The assertion: two messages, two identities across the remount. */
    expect(new Set(latestTurns.map((turn) => turn.id)).size).toBe(2);
  });
});

it.each(['429', 'transport'])('[F5] does not retire a %s failure when a stale read reveals an old equal message', async (mode) => {
  const text = 'repeat this instruction';
  const first = harnessMessage(1, 'userMessage', { content: [{ text: 'Earlier different instruction' }] });
  const oldEqual = harnessMessage(2, 'userMessage', { content: [{ text }] });
  let rows = [first];
  const { client, requests } = setup((request) => {
    if (request.path.includes(HISTORY_PATH)) return ok(rows);
    if (request.path.endsWith('/planner/input')) {
      if (mode === 'transport') throw new Error('Connection lost before request reached server');
      return failure(429, 'rate_limited', 'Request rejected before acceptance');
    }
    return undefined;
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  await write(text);
  expect((await screen.findByRole('alert')).textContent).toContain(mode === '429' ? 'Not sent' : 'Delivery is unconfirmed');
  // The original read was stale; a refresh reveals a pre-existing equal row.
  // Its server timestamp is 2, earlier than this request's Date.now().
  rows = [first, oldEqual, harnessMessage(3, 'agentMessage', { text: 'Previously completed response' })];
  await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
  expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
  await screen.findByText('Previously completed response');
  if (mode === '429') {
    expect(screen.getByRole('alert').textContent).toContain('Not sent');
    expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
    expect(within(drawerElement()).getAllByText(text)).toHaveLength(2);
  } else {
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText('A matching message is visible. Delivery is still unconfirmed.').closest('[role="status"]')?.textContent).toContain('Delivery is still unconfirmed');
    expect(screen.getByRole('button', { name: 'I’ve checked' })).toBeTruthy();
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(within(drawerElement()).getByText(text)).toBeTruthy();
  }
  expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
});


it('restores the open conversation after visiting another Track and remembers an explicit close', async () => {
  const { router } = setup();
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await screen.findByRole('button', { name: 'Close conversation' });
  await act(async () => { await router.navigate({ to: '/track/$trackId', params: { trackId: 'w2' } }); });
  await screen.findAllByRole('heading', { name: 'Bare track', level: 1 });
  expect(screen.queryByRole('button', { name: 'Close conversation' })).toBeNull();
  await act(async () => { await router.navigate({ to: '/track/$trackId', params: { trackId: 'w1' } }); });
  fireEvent.click(await screen.findByRole('button', { name: 'Close conversation' }));
  await act(async () => { await router.navigate({ to: '/track/$trackId', params: { trackId: 'w2' } }); });
  await screen.findAllByRole('heading', { name: 'Bare track', level: 1 });
  await act(async () => { await router.navigate({ to: '/track/$trackId', params: { trackId: 'w1' } }); });
  await screen.findByRole('button', { name: 'Conversation Assistant' });
  expect(screen.queryByRole('button', { name: 'Close conversation' })).toBeNull();
});


it('restores conversation selection across a fresh router without persisting its contents', async () => {
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); } };
  const first = setup(undefined, storage);
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await screen.findByRole('button', { name: 'Close conversation' });
  cleanup();
  first.client.clear();
  setup(undefined, storage);
  expect(await screen.findByRole('complementary', { name: 'Assistant' })).toBeTruthy();
  const saved = [...values.values()].map(value => JSON.parse(value) as string);
  expect(saved).toContain(ASSISTANT_CARD.id);
  expect(saved.every(value => value === ASSISTANT_CARD.id || /^\d+$/.test(value))).toBe(true);
});

it('does not fetch a stored conversation absent from the current Track rows', async () => {
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); } };
  createUiPreferences(storage).setConversation('w1', 'foreign-or-deleted-card');
  const { requests } = setup(undefined, storage);
  await screen.findByRole('button', { name: 'Conversation Assistant' });
  expect(screen.queryByRole('button', { name: 'Close conversation' })).toBeNull();
  expect(requests.some((request) => request.path.includes('foreign-or-deleted-card'))).toBe(false);
});

it.each([false, true])('selects a model before the first conversation message and sends it atomically (bundled=%s)', async bundled => {
  vi.stubGlobal('__NC_BUNDLED__', bundled);
  const access = new RecoveryAccess(); access.change('connected');
  const { requests } = setup(request => {
    if (request.path === '/api/version') return ok({ webCompatVersion: 28, minWebCompatVersion: 28,
      syncEventVersion: 20, dbInstanceId: 'test', conversationCreateModel: true });
    if (request.path === '/api/models?provider=codex') return ok({
      models: [{ id: 'preset-fast', model: 'gpt-5', display_name: 'GPT-5', description: '', is_default: false,
        supported_reasoning_efforts: [{ reasoning_effort: 'low', description: 'Faster' }, { reasoning_effort: 'high', description: 'Thinks longer' }], default_reasoning_effort: 'high' }],
      default: { model: 'gpt-5', reasoning_effort: 'high' }, default_source: 'config_read', source: 'live', fetched_at_ms: 1,
    });
    if (request.method === 'POST' && request.path === CONVERSATIONS) return created(derivedRow('w1', request));
    return undefined;
  }, undefined, bundled ? access : undefined);
  await openDraft();
  fireEvent.click(await screen.findByRole('button', { name: /^Model:/ }));
  fireEvent.click(await screen.findByRole('menuitem', { name: /^GPT-5/ }));
  fireEvent.click(screen.getByRole('button', { name: /^Reasoning effort:/ }));
  fireEvent.click(await screen.findByRole('menuitem', { name: /^high/ }));
  expect(creates(requests, CONVERSATIONS)).toHaveLength(0);
  await write('Use this model from the start');
  await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));
  expect(creates(requests, CONVERSATIONS)[0].body).toEqual({ text: 'Use this model from the start', model: 'gpt-5', reasoning_effort: 'high' });
  expect(requests.filter(request => request.method === 'PUT' && request.path.endsWith('/planner/model'))).toEqual([]);
});

it('keeps first-message selection disabled on an older server that would ignore it', async () => {
  setup(request => request.path === '/api/version'
    ? ok({ webCompatVersion: 28, minWebCompatVersion: 28, syncEventVersion: 20, dbInstanceId: 'old' })
    : undefined);
  await openDraft();
  const model = await screen.findByRole('button', { name: /^Model:/ });
  expect(model.hasAttribute('disabled') || model.getAttribute('aria-disabled') === 'true').toBe(true);
  fireEvent.click(model);
  expect(screen.queryByRole('menuitem', { name: /^Default/ })).toBeNull();
});

it('uses a spinner while a closed conversation runs, a blue unread dot on completion, and no dot after reading', async () => {
  /* Unread follows `lastTurnCompletedAt`: `updatedAt` also moves when a message is
       queued. Working follows the kernel's per-card verdict, not the row's session state. */
  let rows = [assistantRow({ lastTurnCompletedAt: 30 })];
  let cards: ActivityCardWire[] = [];
  const { client } = setup(request => {
    if (request.method === 'GET' && request.path === CONVERSATIONS) return ok(rows);
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_resume: false,
      cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD], overlays: [trackActivityOverlay({ working: cards.length > 0, cards })] });
    return undefined;
  }, receiptStorage());
  const refetch = () => act(async () => {
    await client.invalidateQueries({ queryKey: ['track-conversations', 'w1'] });
    await client.invalidateQueries({ queryKey: ['track', 'w1'] });
  });
  const indicator = () => screen.getByRole('button', { name: /^Conversation Assistant(?:,|$)/ }).closest('li')?.querySelector('[data-nc-activity]');
  await screen.findByRole('button', { name: /^Conversation Assistant(?:,|$)/ });
  expect(indicator()?.getAttribute('data-nc-activity')).toBe('unread');
  fireEvent.click(screen.getByRole('button', { name: /^Conversation Assistant(?:,|$)/ }));
  await waitFor(() => expect(indicator()).toBeNull());
  fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  rows = [{ ...assistantRow(), state: 'turn_pending', updatedAt: 40, lastTurnCompletedAt: 30 }];
  cards = [{ card_id: ASSISTANT_CARD.id, state: 'working' }];
  await refetch();
  await waitFor(() => expect(screen.getByRole('button', { name: /^Conversation Assistant/ }).closest('li')
    ?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity')).toBe('working'));
  // A newer `updatedAt` alone (the reader queued something) is not unread…
  rows = [{ ...assistantRow(), state: 'idle', updatedAt: 45, lastTurnCompletedAt: 30 }];
  cards = [];
  await refetch();
  await waitFor(() => expect(indicator()).toBeNull());
  // …a newer completion is.
  rows = [{ ...assistantRow(), state: 'idle', updatedAt: 50, lastTurnCompletedAt: 50 }];
  await refetch();
  await waitFor(() => expect(indicator()?.getAttribute('data-nc-activity')).toBe('unread'));
  fireEvent.click(screen.getByRole('button', { name: /^Conversation Assistant(?:,|$)/ }));
  await waitFor(() => expect(indicator()).toBeNull());
});

it('keeps the first-message model fixed when a menu opened before sending is selected late', async () => {
  let settle!: (response: ApiTransportResponse) => void;
  const pending = new Promise<ApiTransportResponse>(resolve => { settle = resolve; });
  const { requests } = setup(request => {
    if (request.path === '/api/version') return ok({ webCompatVersion: 28, minWebCompatVersion: 28,
      syncEventVersion: 20, dbInstanceId: 'test', conversationCreateModel: true });
    if (request.path === '/api/models?provider=codex') return ok({
      models: [{ id: 'preset', model: 'chosen-model', display_name: 'Chosen model', description: '', is_default: false,
        supported_reasoning_efforts: [], default_reasoning_effort: 'low' }],
      default: { model: 'default-model', reasoning_effort: null }, default_source: 'config_read', source: 'live', fetched_at_ms: 1,
    });
    if (request.method === 'POST' && request.path === CONVERSATIONS) return pending;
    return undefined;
  });
  await openDraft();
  const modelPicker = await screen.findByRole('button', { name: /^Model:/ });
  await waitFor(() => expect(modelPicker.hasAttribute('disabled') || modelPicker.getAttribute('aria-disabled') === 'true').toBe(false));
  fireEvent.click(modelPicker);
  fireEvent.click(await screen.findByRole('menuitem', { name: 'Chosen model' }));
  fireEvent.click(screen.getByRole('button', { name: 'Model: Chosen model' }));
  await write('Keep my choice');
  await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));
  try {
    fireEvent.click(screen.getByRole('menuitem', { name: /^Default/, hidden: true }));
    expect(screen.getByRole('button', { name: /^Model:/ }).getAttribute('aria-label')).toBe('Model: Chosen model');
  } finally {
    await act(async () => { settle(failure(500, 'internal', 'Try again')); await pending; });
  }
});

it('keeps new replies unread while reopening cached history is still loading', async () => {
  let rows = [assistantRow({ lastTurnCompletedAt: 30 })];
  let holdHistory = false;
  let release!: (response: ApiTransportResponse) => void;
  const pending = new Promise<ApiTransportResponse>(resolve => { release = resolve; });
  const { client, requests } = setup(request => {
    if (request.method === 'GET' && request.path === CONVERSATIONS) return ok(rows);
    if (request.path.includes(HISTORY_PATH)) return holdHistory ? pending
      : ok([harnessMessage(20, 'agentMessage', { type: 'agentMessage', text: 'Previously read answer.' })]);
    return undefined;
  }, receiptStorage());
  const row = () => screen.getByRole('button', { name: /^Conversation Assistant(?:,|$)/ });
  const unread = () => row().closest('li')?.querySelector('[data-nc-activity="unread"]');
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await waitFor(() => expect(unread()).toBeNull());
  fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  rows = [{ ...assistantRow(), updatedAt: 50, lastTurnCompletedAt: 50 }];
  await act(async () => { await client.invalidateQueries({ queryKey: ['track-conversations', 'w1'] }); });
  await waitFor(() => expect(unread()).toBeTruthy());
  const previousReads = requests.filter(request => request.path.includes(HISTORY_PATH)).length;
  holdHistory = true;
  await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
  fireEvent.click(row());
  try {
    await waitFor(() => expect(requests.filter(request => request.path.includes(HISTORY_PATH)).length).toBeGreaterThan(previousReads));
    expect(unread()).toBeTruthy();
  } finally {
    await act(async () => { release(ok([harnessMessage(45, 'agentMessage', { type: 'agentMessage', text: 'New completed answer.' })])); await pending; });
  }
  await waitFor(() => expect(unread()).toBeNull());
});

it('keeps a nonempty conversation read after closing when activity is newer than the last reply', async () => {
  setup(request => {
    if (request.path === CONVERSATIONS) return ok([assistantRow({ title: 'Review receipt', updatedAt: 50 })]);
    if (request.path.includes(HISTORY_PATH)) return ok([harnessMessage(20, 'agentMessage', { type: 'agentMessage', text: 'Read this completed answer.' })]);
    return undefined;
  });
  const row = () => screen.getByRole('button', { name: /^Conversation Review receipt(?:,|$)/ });
  const unread = () => row().closest('li')?.querySelector('[data-nc-activity="unread"]');
  fireEvent.click(await screen.findByRole('button', { name: /^Conversation Review receipt(?:,|$)/ }));
  await screen.findByText('Read this completed answer.');
  await waitFor(() => expect(unread()).toBeNull());
  fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  await waitFor(() => expect(unread()).toBeNull());
});

it('shows a closed Planner working and preserves unread completion until its history is read', async () => {
  let status = 'turn_pending';
  let activityAt = 50;
  /* Working is the kernel's verdict in the activity overlay; `status` is the
       session reading no indicator reads. */
  let cards: ActivityCardWire[] = [{ card_id: PLANNER_CARD.id, state: 'working' }];
  /* `CardRuntimeView.last_turn_completed_ms`: absent while the first turn is still running. */
  let lastTurnCompletedMs: number | undefined;
  const { client } = setup(request => {
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_resume: false,
      cards: [{ ...PLANNER_CARD, runtime: { worker_session_id: 'planner-live', kind: 'shared-spec', status, updated_at_ms: activityAt,
        ...(lastTurnCompletedMs === undefined ? {} : { last_turn_completed_ms: lastTurnCompletedMs }) } }],
      overlays: [trackActivityOverlay({ working: cards.length > 0, activity_at_ms: lastTurnCompletedMs ?? null, cards })] });
    if (request.path.startsWith('/api/cards/card-planner/harness/items')) return ok([
      harnessMessage(40, 'agentMessage', { type: 'agentMessage', text: 'Completed planner answer.' }),
    ]);
    return undefined;
  }, receiptStorage());
  const row = () => screen.getByRole('button', { name: /^Conversation Planner chat(?:,|$)/ });
  const indicator = () => row().closest('li')?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity');
  await screen.findByRole('button', { name: /^Conversation Planner chat(?:,|$)/ });
  expect(indicator()).toBe('working');
  status = 'idle'; activityAt = 60; lastTurnCompletedMs = 60; cards = [];
  await act(() => {
    const plan = invalidationPlanFor({ ev: 'harness.phase.changed', data: {
      worker_session_id: 'planner-live', card_id: 'card-planner', track_id: 'w1',
      old_phase: 'turn_running', new_phase: 'turn_completed',
    } });
    applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
    return Promise.resolve();
  });
  await waitFor(() => expect(indicator()).toBe('unread'));
  fireEvent.click(row());
  await screen.findByText('Completed planner answer.');
  await waitFor(() => expect(indicator()).toBeUndefined());
  fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  expect(indicator()).toBeUndefined();
  const completed = () => act(() => {
    const plan = invalidationPlanFor({ ev: 'harness.phase.changed', data: {
      worker_session_id: 'planner-live', card_id: 'card-planner', track_id: 'w1',
      old_phase: 'turn_running', new_phase: 'turn_completed',
    } });
    applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
    return Promise.resolve();
  });
  // The session's `updated_at_ms` moving on its own (the reader queued a
  // message, say) is not a completion and does not relight the row…
  activityAt = 70;
  await completed();
  const cachedActivityAt = () => client.getQueryData<{ cards: { runtime?: { updated_at_ms?: number } }[] }>(['track', 'w1'])
    ?.cards[0]?.runtime?.updated_at_ms;
  await waitFor(() => expect(cachedActivityAt()).toBe(70));
  expect(indicator()).toBeUndefined();
  // …a newer `last_turn_completed_ms` does.
  activityAt = 80; lastTurnCompletedMs = 80;
  await completed();
  await waitFor(() => expect(indicator()).toBe('unread'));
});

/* Both kinds of row take their dot from `activity.cards`; `state` / `runtime.status`
 * is the session reading the harness leaves at `turn_pending` long after a turn ended. */
it('conversation rows read activity.cards, not session state', async () => {
  let cards: ActivityCardWire[] = [];
  const { client } = setup(request => {
    if (request.method === 'GET' && request.path === CONVERSATIONS) return ok([assistantRow({ state: 'turn_pending' })]);
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_resume: false,
      cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD], overlays: [trackActivityOverlay({ cards })] });
    return undefined;
  }, receiptStorage());
  const row = () => screen.getByRole('button', { name: /^Conversation Assistant(?:,|$)/ });
  const indicator = () => row().closest('li')?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity');
  await screen.findByRole('button', { name: /^Conversation Assistant(?:,|$)/ });
  expect(indicator()).toBeUndefined();
  expect(row().getAttribute('aria-label')).toBe('Conversation Assistant');
  expect(row().hasAttribute('aria-describedby')).toBe(false);

  cards = [{ card_id: ASSISTANT_CARD.id, state: 'working' }];
  await act(async () => { await client.invalidateQueries({ queryKey: ['track', 'w1'] }); });
  await waitFor(() => expect(indicator()).toBe('working'));
  expect(row().getAttribute('aria-label')).toBe('Conversation Assistant, working');
  expect(row().hasAttribute('aria-describedby')).toBe(false);

  cards = [{ card_id: ASSISTANT_CARD.id, state: 'failed' }];
  await act(async () => { await client.invalidateQueries({ queryKey: ['track', 'w1'] }); });
  await waitFor(() => expect(indicator()).toBe('failed'));
  expect(row().getAttribute('aria-label')).toBe('Conversation Assistant');
  expect(document.getElementById(row().getAttribute('aria-describedby') ?? '')?.textContent).toBe('Needs attention');

  cards = [{ card_id: ASSISTANT_CARD.id, state: 'input' }];
  await act(async () => { await client.invalidateQueries({ queryKey: ['track', 'w1'] }); });
  await waitFor(() => expect(indicator()).toBe('attention'));
  expect(document.getElementById(row().getAttribute('aria-describedby') ?? '')?.textContent).toBe('Needs input');
});

it('the injected planner row reads activity.cards and last_turn_completed_ms', async () => {
  let cards: ActivityCardWire[] = [];
  let updatedAtMs = 50;
  let lastTurnCompletedMs: number | undefined;
  const { client } = setup(request => {
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_resume: false,
      cards: [{ ...PLANNER_CARD, runtime: { worker_session_id: 'planner-live', kind: 'codex', status: 'turn_pending',
        updated_at_ms: updatedAtMs, ...(lastTurnCompletedMs === undefined ? {} : { last_turn_completed_ms: lastTurnCompletedMs }) } }],
      overlays: [trackActivityOverlay({ cards })] });
    if (request.path.startsWith('/api/cards/card-planner/harness/items')) return ok([
      harnessMessage(40, 'agentMessage', { type: 'agentMessage', text: 'Completed planner answer.' }),
    ]);
    return undefined;
  }, receiptStorage());
  const row = () => screen.getByRole('button', { name: /^Conversation Planner chat(?:,|$)/ });
  const indicator = () => row().closest('li')?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity');
  const refetch = () => act(async () => { await client.invalidateQueries({ queryKey: ['track', 'w1'] }); });
  await screen.findByRole('button', { name: /^Conversation Planner chat(?:,|$)/ });
  // `runtime.status: 'turn_pending'` with no verdict is not working.
  expect(indicator()).toBeUndefined();
  expect(row().getAttribute('aria-label')).toBe('Conversation Planner chat');

  cards = [{ card_id: PLANNER_CARD.id, state: 'working' }];
  await refetch();
  await waitFor(() => expect(indicator()).toBe('working'));
  expect(row().getAttribute('aria-label')).toBe('Conversation Planner chat, working');

  // Read it once, so the receipt has a point to compare against…
  cards = [];
  lastTurnCompletedMs = 50;
  await refetch();
  await waitFor(() => expect(indicator()).toBe('unread'));
  fireEvent.click(row());
  await screen.findByText('Completed planner answer.');
  await waitFor(() => expect(indicator()).toBeUndefined());
  fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  // …then `updated_at_ms` moving on its own is not unread…
  updatedAtMs = 70;
  await refetch();
  await waitFor(() => expect(client.getQueryData<{ cards: { runtime?: { updated_at_ms?: number } }[] }>(['track', 'w1'])
    ?.cards[0]?.runtime?.updated_at_ms).toBe(70));
  expect(indicator()).toBeUndefined();
  // …and a newer completion is.
  lastTurnCompletedMs = 80;
  await refetch();
  await waitFor(() => expect(indicator()).toBe('unread'));

  cards = [{ card_id: PLANNER_CARD.id, state: 'failed' }];
  await refetch();
  await waitFor(() => expect(indicator()).toBe('failed'));
  expect(row().getAttribute('aria-label')).toBe('Conversation Planner chat');
  expect(document.getElementById(row().getAttribute('aria-describedby') ?? '')?.textContent).toBe('Needs attention');
});

it('an edited conversation retry cannot cross recovery after its reconciliation read completed', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected');
  const { requests, client } = setup(request => request.method === 'POST' && request.path === CONVERSATIONS
    ? failure(500, 'internal', 'unconfirmed first write') : undefined, undefined, access);
  await screen.findByRole('button', { name: 'Conversation Planner chat' });
  await openDraft(); await write('original words');
  await screen.findByRole('button', { name: 'Try again' });
  const fetchQuery = client.fetchQuery.bind(client);
  const fetch = vi.spyOn(client, 'fetchQuery').mockImplementation(async options => {
    const rows = await fetchQuery(options);
    // Deliver the real result, with recovery occurring at the promise boundary
    // between Query's completed read and the caller's continuation.
    access.invalidate('recovering'); access.change('connected');
    return rows;
  });
  try {
    await write('edited words');
    await waitFor(() => expect(fetch).toHaveBeenCalled());
    await screen.findByRole('button', { name: 'Try again' });
    expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
  } finally { fetch.mockRestore(); }
});
