// @vitest-environment jsdom
// Starting a conversation on a track, driven through the real router; and what the
// session registry may remember about one, driven through the real store.

import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { SEND_RETRIES } from '../../../../core/domain/conversation-delivery.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { onlineManager, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useEffect, useLayoutEffect } from 'react';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import { applyEventEffects } from '../events/query-invalidation-adapter.ts';
import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import {
  CONVERSATION_CREATE_TEXT, conversationCreateUnknownText, MAX_ATTACHMENTS_PER_MESSAGE, trackConversationCardId,
} from '../../../../core/domain/conversation.ts';
import { ConversationProvider, useConversationRegistry } from '../conversations/public.tsx';
import { createUiPreferences, type UiPreferenceStorage } from '../providers/ui-preferences.tsx';
import { DATABASE_ID_KEY } from '../../../../core/keys/storage.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter, useConversationStore } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

/* A send's automatic retries wait no real time here; how long they back off is not under test. */
vi.mock('../../../../core/domain/recovery/access.ts', async (importOriginal) => ({
  ...await importOriginal<typeof import('../../../../core/domain/recovery/access.ts')>(),
  recoveryDelay: () => 0,
}));

/** Text in the drawer outside its desktop header, which paints the conversation's name (often its first message). */
const TRANSCRIPT_TEXT = { ignore: 'script, style, [data-nc-drawer] > header *' };

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = { id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
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
type ActivityItemWire = { source: 'ask' | 'planner_down'; key: string; text: string; at_ms: number };
type ActivityCardWire = { card_id: string; state: 'working' | 'input' | 'failed' };
const trackActivityOverlay = (payload: Partial<{
  working: boolean; attention: 'none' | 'input' | 'failed'; activity_at_ms: number | null;
  items: ActivityItemWire[]; cards: ActivityCardWire[];
}> = {}, trackId = 'w1') => ({
  id: `activity-${trackId}`, plugin_id: 'kernel', entity_kind: 'track', entity_id: trackId, kind: 'activity',
  payload: { schemaVersion: 2, working: false, attention: 'none', activity_at_ms: null, items: [], cards: [], ...payload },
  updated_at: 3,
});
/** The Planner's ask, in its words, as the projector lists it. */
const askItem = (text: string, atMs: number): ActivityItemWire =>
  ({ source: 'ask', key: `ask:ratify:${atMs}`, text, at_ms: atMs });
/** The Planner stopped: its failure reason, as the projector lists it. */
const plannerDownItem = (text: string, atMs: number): ActivityItemWire =>
  ({ source: 'planner_down', key: `planner_down:${atMs}`, text, at_ms: atMs });
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
    turn_id: null, turn_error_text: null, item_uuid: null, item_type: itemType, method: 'item/completed',
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
const runIdle = () => ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });

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
          track: TRACK, can_reopen: false, can_close: true,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD], overlays: [],
        });
      }
      if (request.path === '/api/tracks/w2') return ok({
        track: BARE_TRACK, can_reopen: false, can_close: true, cards: [], overlays: [],
      });
      if (request.path === CONVERSATIONS) return ok([assistantRow()]);
      if (request.path === BARE_CONVERSATIONS) return ok([]);
      if (request.path.includes(HISTORY_PATH)) return ok([]);
      /* Both card endpoints echo the card in the path, as the kernel does; a fixed id
         would be a trap for the first case that reads the field. */
      if (request.path.endsWith('/planner/run')) return ok({ card_id: pathCardId(request.path), worker_session_id: 'r', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
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

  it('both notification kinds show the kernel\'s words and a row click opens the Planner composer', async () => {
    const mount = () => setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_reopen: false, can_close: true,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackActivityOverlay({ attention: 'failed', items: [
            plannerDownItem('400: The gpt-6-astra model requires a newer version of Codex.', 5),
            askItem('Merge **PR #1811** now, or hold it?', 4),
          ] })],
        })
      : undefined);
    mount();
    const notice = await screen.findByRole('region', { name: 'Notifications' });
    const rows = within(notice).getAllByRole('listitem');
    expect(rows.map((row) => row.getAttribute('data-nc-notification-state'))).toEqual(['planner-down', 'ask']);
    expect(within(rows[0]).getByText("Planner can't continue")).toBeTruthy();
    expect(within(rows[0]).getByText('400: The gpt-6-astra model requires a newer version of Codex.')).toBeTruthy();
    expect(within(rows[1]).getByText('Needs your answer')).toBeTruthy();
    expect(within(rows[1]).getByText('PR #1811').tagName).toBe('STRONG');
    expect(within(rows[1]).getByText('PR #1811').parentElement?.textContent).toBe('Merge PR #1811 now, or hold it?');
    /* Each row's click, on a fresh mount: the Planner's composer opens focused and the aside compacts beside it. */
    for (const [index, [state, name]] of ([['planner-down', /^Open the Planner: /], ['ask', /^Answer the Planner: /]] as const).entries()) {
      if (index > 0) {
        cleanup();
        window.history.pushState({}, '', `${APP_BASEPATH}/track/w1`);
        mount();
      }
      const row = within(await screen.findByRole('region', { name: 'Notifications' })).getAllByRole('listitem')
        .find((candidate) => candidate.getAttribute('data-nc-notification-state') === state);
      fireEvent.click(within(row!).getByRole('button', { name }));
      expect(await screen.findByRole('complementary', { name: 'Planner chat' })).toBeTruthy();
      await waitFor(() => expect(screen.getByRole('combobox', { name: 'Message' })).toBe(document.activeElement));
      expect(screen.getByRole('region', { name: 'Notifications' })
        .getAttribute('data-nc-notification-mode')).toBe('compact');
      expect(screen.getByRole('region', { name: 'Notifications' }).querySelector('strong')).toBeNull();
      expect(window.location.search).not.toContain('card=');
    }
  });

  it('Dismiss posts the item key', async () => {
    let dismissed = false;
    const { client, requests } = setup((request) => {
      if (request.path === '/api/tracks/w1/activity/dismissals') {
        dismissed = true;
        return { status: 204, statusText: 'No Content', body: undefined };
      }
      return request.path === '/api/tracks/w1'
        ? ok({
            track: TRACK, can_reopen: false, can_close: true,
            cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
            overlays: [trackActivityOverlay({ attention: 'failed', items: [
              plannerDownItem('unexpected status 403 Forbidden', 5),
              ...(dismissed ? [] : [askItem('Merge PR #1811 now, or hold it?', 4)]),
            ] })],
          })
        : undefined;
    });
    const askRow = within(await screen.findByRole('region', { name: 'Notifications' })).getAllByRole('listitem')
      .find((row) => row.getAttribute('data-nc-notification-state') === 'ask');
    fireEvent.click(within(askRow!).getByRole('button', { name: /^Dismiss: Needs your answer: / }));
    await waitFor(() => expect(dismissed).toBe(true));
    expect(requests.filter((request) => request.path === '/api/tracks/w1/activity/dismissals'))
      .toEqual([expect.objectContaining({ method: 'POST', body: { key: 'ask:ratify:4' } })]);
    /* No optimistic removal: the row stays until the projector's `overlay.set` refreshes the track. */
    const notice = screen.getByRole('region', { name: 'Notifications' });
    expect(within(notice).getAllByRole('listitem')).toHaveLength(2);
    await act(() => {
      const plan = invalidationPlanFor({ ev: 'overlay.set', data: {
        id: 'activity-w1', plugin_id: 'kernel', entity_kind: 'track', entity_id: 'w1', kind: 'activity',
        payload: {}, updated_at: 4,
      } });
      applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
      return Promise.resolve();
    });
    await waitFor(() => expect(within(screen.getByRole('region', { name: 'Notifications' }))
      .getAllByRole('listitem').map((row) => row.getAttribute('data-nc-notification-state'))).toEqual(['planner-down']));
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('counts an ask and planner down as two notifications', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_reopen: false, can_close: true,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackActivityOverlay({ attention: 'failed',
            items: [plannerDownItem('boom', 5), askItem('Which region?', 4)],
            cards: [{ card_id: PLANNER_CARD.id, state: 'failed' }] })],
        })
      : undefined);

    const notice = await screen.findByRole('region', { name: 'Notifications' });
    expect(within(notice).getByText('Waiting on you').nextElementSibling?.textContent).toBe('2');
    fireEvent.click(within(notice).getByRole('button', { name: 'Collapse notifications' }));
    expect(await screen.findByRole('button', { name: 'Open 2 notifications' })).toBeTruthy();
  });

  it('ignores a retired kernel/card/status row and a plugin-authored activity row', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_reopen: false, can_close: true,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [
            cardStatusOverlay(WORKER_CARD.id, 'AwaitingInput', 5),
            { ...trackActivityOverlay({ attention: 'input', items: [askItem('Which region?', 4)] }),
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
    await waitFor(() => expect(within(drawer).getAllByText('repeat me', TRANSCRIPT_TEXT)).toHaveLength(2));

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    holdReopen = true;
    client.removeQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation repeat me/ }));
    const reopened = await screen.findByRole('complementary', { name: 'repeat me' });
    expect(within(reopened).getAllByText('repeat me', TRANSCRIPT_TEXT)).toHaveLength(2);
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
    expect((await screen.findByRole('alert')).textContent).toContain('The conversation history could not be loaded.');
    /* A 503's text is the server's: only the fixed sentence shows. */
    expect(screen.getByRole('alert').textContent).not.toContain('history unavailable');
    expect(screen.queryByText(/Nothing said yet/)).toBeNull();
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(requests.some((request) => request.path.endsWith('/planner/input'))).toBe(false);

    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(reads).toBe(2);
  });

  /* #2175 M1a: both reads failing is one notice with one Try again, which reads both again. */
  it('folds a failed history read and a failed run read into one notice whose Try again re-reads both', async () => {
    let failing = true;
    const { requests } = setup((request) => {
      if (!failing) return undefined;
      if (request.path.includes(HISTORY_PATH)) return failure(503, 'unavailable', 'history unavailable');
      if (request.path.endsWith('/planner/run')) return failure(503, 'unavailable', 'run unavailable');
      return undefined;
    });
    const reads = (suffix: string) => requests.filter((request) => request.path.includes(suffix)).length;
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => {
      const text = within(drawerElement()).getAllByRole('alert').map((alert) => alert.textContent).join(' | ');
      expect(text).toContain('The conversation history could not be loaded.');
      expect(text).toContain('The conversation’s status could not be loaded.');
    });
    expect(within(drawerElement()).getAllByRole('alert')).toHaveLength(1);
    expect(within(drawerElement()).getAllByRole('button', { name: 'Try again' })).toHaveLength(1);

    const before = { history: reads(HISTORY_PATH), run: reads('/planner/run') };
    failing = false;
    fireEvent.click(within(drawerElement()).getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(within(drawerElement()).queryByRole('alert')).toBeNull());
    expect(reads(HISTORY_PATH)).toBeGreaterThan(before.history);
    expect(reads('/planner/run')).toBeGreaterThan(before.run);
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
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
    /* A 503 may follow a stored message: unconfirmed, and in no words of the answer's. */
    expect(within(await screen.findByRole('alert')).getByText('Delivery is unconfirmed.')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Planner chat' }));
    expect(screen.queryByText('Delivery is unconfirmed.')).toBeNull();
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

  /* A refusal gives the words back to the composer of the conversation they were sent from (#2068), never to the
   * one shown when it lands. */
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

  /* #2068: a refusal is settled by giving the words back; the thread keeps nothing of it that a later press could drop. */
  it('[F4] keeps a typed refusal in the composer alone, across reopening and a newer draft', async () => {
    let refuse = true;
    const { requests } = setup((request) => request.path.endsWith('/planner/input') && refuse
      ? failure(409, 'planner_harness_runtime_superseded', 'Your message was not stored') : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('Original typed refusal');
    await waitFor(() => expect(messageField().textContent).toBe('Original typed refusal'));
    /* Once, in the composer: no copy of it is drawn in the thread. */
    expect(within(drawerElement()).getAllByText('Original typed refusal', TRANSCRIPT_TEXT)).toHaveLength(1);
    expect(drawerElement().querySelector('[data-nc-turn="you"]')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
    await typeInto(messageField(), 'A newer unsent draft');
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().textContent).toBe('A newer unsent draft'));
    expect(within(drawerElement()).queryByText('Original typed refusal', TRANSCRIPT_TEXT)).toBeNull();
    refuse = false;
    await act(async () => { fireEvent.keyDown(messageField(), { key: 'Enter' }); await Promise.resolve(); });
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    expect(messageField().textContent).toBe('');
    expect(requests.filter((request) => request.path.endsWith('/planner/input')).map((request) => request.body))
      .toEqual([{ text: 'Original typed refusal' }, { text: 'A newer unsent draft' }]);
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
    expect(within(drawerElement()).getByText(text, TRANSCRIPT_TEXT)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    expect(within(drawerElement()).getByText(text, TRANSCRIPT_TEXT)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(requests.filter((request) => request.path.endsWith('/planner/input')).map((request) => request.body))
      .toEqual([{ text }, { text }]);
    expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1);
  });

  it.each(['rejected', 'unknown'] as const)(
    '[F4] never labels the %s working-turn submission as queued, including after reopen', async (outcome) => {
      const text = `Keep ${outcome} queued attempt`;
      const { requests } = setup((request) => {
        if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
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
      expect(within(drawerElement()).getByText(text, TRANSCRIPT_TEXT)).toBeTruthy();
      expect(document.querySelector('[data-nc-queued]')).toBeNull();
      expect(document.querySelector('[data-nc-queued-note]')).toBeNull();
      expect(requests.filter((request) => request.path.endsWith('/planner/input')))
        .toHaveLength(outcome === 'unknown' ? SEND_RETRIES + 1 : 1);
      fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
      fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
      expect(within(drawerElement()).getByText(text, TRANSCRIPT_TEXT)).toBeTruthy();
      expect(document.querySelector('[data-nc-queued-note]')).toBeNull();
    },
  );

  it('[F4] marks a working-turn message queued only after its POST is acknowledged', async () => {
    let resolve!: (response: ApiTransportResponse) => void;
    const held = new Promise<ApiTransportResponse>((answer) => { resolve = answer; });
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
      if (request.path.endsWith('/planner/input')) return held;
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('button', { name: 'Stop' });
    await typeInto(messageField(), 'Waiting for the server');
    await submit();
    await waitFor(() => expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1));
    expect(within(drawerElement()).getByText('Waiting for the server', TRANSCRIPT_TEXT)).toBeTruthy();
    expect(document.querySelector('[data-nc-queued-note]')).toBeNull();
    await act(async () => { resolve(inputAccepted()); await held; });
    await screen.findByText('Queued · sends when this turn ends');
    expect(messageField().getAttribute('contenteditable')).toBe('true');
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
    '[F5] retries a %s failure under its key, then offers Try again without asking', async (mode) => {
      const text = 'repeat the same request';
      let failing = true;
      let rows: ReturnType<typeof harnessMessage>[] = [];
      const { requests } = setup((request) => {
        if (request.path.includes(HISTORY_PATH)) return ok(rows);
        if (request.path.endsWith('/planner/input')) {
          /* Stored, and drained into the transcript the refresh after the 200 reads. */
          if (!failing) { rows = [harnessMessage(1, 'userMessage', { content: [{ text }] })]; return inputAccepted(); }
          if (mode === 'transport') throw new Error('response dropped');
          return mode === '503' ? failure(503, 'unavailable', 'upstream unavailable') : ok({});
        }
        return undefined;
      });
      const inputs = () => requests.filter((request) => request.path.endsWith('/planner/input'));
      fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
      await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
      await write(text);
      expect((await screen.findByRole('alert')).textContent).toContain('Delivery is unconfirmed');
      expect(inputs()).toHaveLength(SEND_RETRIES + 1);
      const key = inputs()[0]?.headers?.['Idempotency-Key'];
      expect(key).toBeTruthy();
      expect(inputs().every((request) => request.headers?.['Idempotency-Key'] === key)).toBe(true);
      /* An edited message would be a new send under a new key while the first may have arrived. */
      expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
      expect(messageField().getAttribute('contenteditable')).toBe('false');

      failing = false;
      fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
      await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
      expect(screen.queryByRole('dialog')).toBeNull();
      expect(inputs()).toHaveLength(SEND_RETRIES + 2);
      expect(inputs().at(-1)?.headers?.['Idempotency-Key']).toBe(key);
      expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1);
    },
  );

  it.each([
    [401, 'session_expired'], [403, 'forbidden'], [429, 'rate_limited'],
  ] as const)('[F5] keeps a dropped answer unknown when the retry is answered %s', async (status, code) => {
    const text = `Lost, then ${status}`;
    let attempts = 0;
    const { requests } = setup((request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      attempts += 1;
      if (attempts === 1) throw new Error('response dropped');
      return attempts === 2 ? failure(status, code, 'Answered without handling it') : inputAccepted();
    });
    const inputs = () => requests.filter((request) => request.path.endsWith('/planner/input'));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    /* The first attempt may have been stored; an answer to the second does not say otherwise. */
    expect((await screen.findByRole('alert')).textContent).toContain('Delivery is unconfirmed');
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
    expect(inputs()).toHaveLength(2);
    const key = inputs()[0]?.headers?.['Idempotency-Key'];
    expect(key).toBeTruthy();
    expect(inputs()[1]?.headers?.['Idempotency-Key']).toBe(key);

    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(inputs()).toHaveLength(3));
    expect(inputs()[2]?.headers?.['Idempotency-Key']).toBe(key);
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
  });

  it.each([
    [401, 'session_expired'], [403, 'forbidden'], [429, 'rate_limited'], [409, 'planner_harness_dormant'],
  ] as const)('[F5] keeps a spent unknown send unknown when its Try again is answered %s %s', async (status, code) => {
    const text = `Spent, then ${code}`;
    let answer: 'drop' | 'refuse' | 'accept' = 'drop';
    const { requests } = setup((request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      if (answer === 'drop') throw new Error('response dropped');
      return answer === 'refuse' ? failure(status, code, 'Answered without storing it') : inputAccepted();
    });
    const inputs = () => requests.filter((request) => request.path.endsWith('/planner/input'));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    expect((await screen.findByRole('alert')).textContent).toContain('Delivery is unconfirmed');
    expect(inputs()).toHaveLength(SEND_RETRIES + 1);
    const key = inputs()[0]?.headers?.['Idempotency-Key'];

    /* Try again resumes the op: whatever this answer says, an earlier attempt may have been stored. */
    answer = 'refuse';
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(inputs()).toHaveLength(SEND_RETRIES + 2));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Delivery is unconfirmed'));
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
    expect(inputs().every((request) => request.headers?.['Idempotency-Key'] === key)).toBe(true);

    answer = 'accept';
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    expect(inputs().at(-1)?.headers?.['Idempotency-Key']).toBe(key);
  });

  it('[F5] reconciles a resumed send whose first attempt was stored and drained, also after reopening', async () => {
    const text = 'Stored before the answer was lost';
    const earlier = harnessMessage(1, 'agentMessage', { text: 'Earlier answer' });
    let rows = [earlier];
    let accept = false;
    const { client } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) return ok(rows);
      if (!request.path.endsWith('/planner/input')) return undefined;
      if (!accept) throw new Error('response dropped');
      return inputAccepted();
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByText('Earlier answer');
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    await screen.findByRole('alert');
    /* The kernel had stored it after all; the queue drained into the transcript. */
    rows = [earlier, harnessMessage(2, 'userMessage', { content: [{ text }] }), harnessMessage(3, 'agentMessage', { text: 'Reply to it' })];
    await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
    await screen.findByText('Reply to it');

    accept = true;
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    await waitFor(() => expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Stored before/ }));
    await screen.findByText('Reply to it');
    expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1);
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  });

  /* The server holds the op's entry, or not: what a replayed 200 claims is not trusted for a once-unknown op. */
  function unknownOpServer(phase: 'turn_running' | 'idle', text: string) {
    const entry = { entry_id: 'entry-9', text, rev: 0, queued_at_ms: 5 };
    const state = { queued: true, accept: false };
    const view = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase, model: null, reasoning_effort: null,
          blocked_reason: null, running_turn: null, pending: state.queued ? [entry] : [], pending_overflow: 0 });
      }
      if (!request.path.endsWith('/planner/input')) return undefined;
      if (!state.accept) throw new Error('response dropped');
      return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', entry_id: entry.entry_id });
    });
    return { ...view, state };
  }

  async function sendUntilSpent(text: string) {
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    expect((await screen.findByRole('alert')).textContent).toContain('Delivery is unconfirmed');
  }

  it.each(['turn_running', 'idle'] as const)(
    '[F5] shows nothing for a resumed send whose stored entry was deleted (%s at the press)', async (phase) => {
      const text = `Deleted before Try again, ${phase}`;
      const { client, state } = unknownOpServer(phase, text);
      await sendUntilSpent(text);
      /* Another tab deleted the queued entry; the server still replays its id under the key. */
      state.queued = false;
      await act(async () => { await client.invalidateQueries(); });
      state.accept = true;
      fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
      await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
      await waitFor(() => expect(within(drawerElement()).queryAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(0));
      await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));

      fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
      fireEvent.click(await screen.findByRole('button', { name: /Conversation (Assistant|Deleted before)/ }));
      await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
      expect(within(drawerElement()).queryAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(0);
    },
  );

  it('[F5] shows a resumed send once, from the queue, while its stored entry is still queued', async () => {
    const text = 'Still queued at Try again';
    const { state } = unknownOpServer('turn_running', text);
    await sendUntilSpent(text);
    state.accept = true;
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="entry-9"]')?.textContent).toContain(text));
    await waitFor(() => expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1));
    expect(messageField().getAttribute('contenteditable')).toBe('true');
  });

  it('[#2068] dismisses a spent unknown send without putting its words or images back; a read then shows it if it was stored', async () => {
    const text = 'Dismissed, maybe delivered';
    let rows: ReturnType<typeof harnessMessage>[] = [];
    let dropping = true;
    const { client, requests } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) return ok(rows);
      if (request.path.endsWith('/planner/run')) {
        return ok({ card_id: pathCardId(request.path), worker_session_id: 'r', phase: 'idle', model: null, reasoning_effort: null,
          blocked_reason: null, attachments_supported: true, running_turn: null });
      }
      if (request.path.endsWith('/planner/attachments')) {
        return ok({ attachmentId: ATTACHMENT_ID, contentType: 'image/png', size: 4,
          url: `/api/cards/${pathCardId(request.path)}/planner/attachments/${ATTACHMENT_ID}` });
      }
      if (request.path.endsWith('/planner/input') && dropping) throw new Error('response dropped');
      return undefined;
    });
    const inputs = () => requests.filter((request) => request.path.endsWith('/planner/input'));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await attachAnImage();
    await waitFor(() => expect(composerImages()).toHaveLength(1));
    await write(text);
    expect((await screen.findByRole('alert')).textContent).toContain('Delivery is unconfirmed');
    expect(inputBodies(requests)[0]).toEqual({ text, attachments: [ATTACHMENT_ID] });
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    expect(within(drawerElement()).queryAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(0);
    expect(messageField().getAttribute('contenteditable')).toBe('true');
    expect(messageField().textContent).toBe('');
    /* Its image may be bound to a stored message: it leaves the composer with the send, as a delivery takes it. */
    expect(composerImages()).toEqual([]);
    expect(inputs()).toHaveLength(SEND_RETRIES + 1);
    /* It had been stored after all: the next read shows it, once. */
    rows = [harnessMessage(1, 'userMessage', { content: [{ text }] })];
    await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
    await waitFor(() => expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1));
    expect(inputs()).toHaveLength(SEND_RETRIES + 1);
    /* The next, unrelated message carries nothing of the dismissed one. */
    dropping = false;
    await write('Something else');
    await waitFor(() => expect(inputs()).toHaveLength(SEND_RETRIES + 2));
    expect(inputBodies(requests).at(-1)).toEqual({ text: 'Something else' });
  });

  it('[#2068] draws a spent unknown send once when its message drains, and its Try again leaves one', async () => {
    const text = 'Drained while unconfirmed';
    let rows: ReturnType<typeof harnessMessage>[] = [];
    let accept = false;
    const { client } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) return ok(rows);
      if (!request.path.endsWith('/planner/input')) return undefined;
      if (!accept) throw new Error('response dropped');
      return inputAccepted();
    });
    await sendUntilSpent(text);
    rows = [harnessMessage(1, 'userMessage', { content: [{ text }] })];
    await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
    await waitFor(() => expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1));
    expect(screen.getByRole('alert').textContent).toContain('Delivery is unconfirmed');
    accept = true;
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1);
  });

  it('[#2068] keeps a send answered after an unknown attempt shown until a read started after its answer lands', async () => {
    const text = 'Answered on the second attempt';
    let attempts = 0;
    let rows: ReturnType<typeof harnessMessage>[] = [];
    /* Reads and the second attempt are answered by hand, so which read started before the 200 is decided here. */
    const held: { input?: () => void; history?: () => void } = {};
    let holdHistory = false;
    const { client } = setup(async (request) => {
      if (request.path.includes(HISTORY_PATH)) {
        /* What the server holds when the read starts. */
        const read = rows;
        if (holdHistory) await new Promise<void>((release) => { held.history = release; });
        return ok(read);
      }
      if (!request.path.endsWith('/planner/input')) return undefined;
      attempts += 1;
      if (attempts === 1) throw new Error('response dropped');
      await new Promise<void>((release) => { held.input = release; });
      rows = [harnessMessage(1, 'userMessage', { content: [{ text }] })];
      return inputAccepted();
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    await waitFor(() => expect(held.input).toBeDefined());
    /* A transcript read that starts before the answer and lands after it, with the message not yet in it. */
    holdHistory = true;
    const historyKey = cachedHistoryKey(client, ASSISTANT_CARD.id);
    void client.invalidateQueries({ queryKey: historyKey });
    await waitFor(() => expect(held.history).toBeDefined());
    const early = held.history;
    /* Closed, so the answer's refresh starts no new read; it cancels the early one, and the data from before the
       answer is what the drawer reopens on. */
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    await act(async () => { held.input?.(); await Promise.resolve(); });
    held.history = undefined;
    await act(async () => { early?.(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(client.getQueryState(historyKey)?.status).toBe('success');
    /* Reopened while its own read is still out: the early read cannot stand for the answer, so the message stays. */
    fireEvent.click(await screen.findByRole('button', { name: /^Conversation (Assistant|Answered on)/ }));
    await waitFor(() => expect(held.history).toBeDefined());
    expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1);
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    holdHistory = false;
    await act(async () => { held.history?.(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1);
  });

  it('[#2043] keeps the composer closed while a send is out, even once a read shows its message', async () => {
    const text = 'Stored before its answer came';
    let rows: ReturnType<typeof harnessMessage>[] = [];
    let answer!: () => void;
    const answered = new Promise<void>((done) => { answer = done; });
    const { client, requests } = setup(async (request) => {
      if (request.path.includes(HISTORY_PATH)) return ok(rows);
      if (!request.path.endsWith('/planner/input')) return undefined;
      await answered;
      return inputAccepted();
    });
    const inputs = () => requests.filter((request) => request.path.endsWith('/planner/input'));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    await waitFor(() => expect(inputs()).toHaveLength(1));
    /* An event-driven read shows the stored message while its answer is still out. */
    rows = [harnessMessage(1, 'userMessage', { content: [{ text }] })];
    await act(async () => { await client.invalidateQueries({ queryKey: cachedHistoryKey(client, ASSISTANT_CARD.id) }); });
    await waitFor(() => expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1));
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    await typeInto(messageField(), 'A second message');
    await submit();
    expect(messageField().textContent).toBe('A second message');
    expect(inputs()).toHaveLength(1);
    await act(async () => { answer(); await answered; });
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(messageField().textContent).toBe('A second message');
    expect(inputs()).toHaveLength(1);
  });

  it('[#2068] retires a replayed send by a run read started after its answer, not by a first read still out before it', async () => {
    const text = 'Replayed for an entry deleted meanwhile';
    let attempts = 0;
    let runReads = 0;
    const { requests } = setup(async (request) => {
      if (request.path.endsWith('/planner/run')) {
        runReads += 1;
        /* The first run read never lands: the query layer would hand it back in place of the read the answer asks for. */
        if (runReads === 1) await new Promise<void>(() => undefined);
        return undefined;
      }
      if (!request.path.endsWith('/planner/input')) return undefined;
      attempts += 1;
      if (attempts === 1) throw new Error('response dropped');
      return inputAccepted();
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    await waitFor(() => expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(2));
    /* No read shows it (the entry was disposed of), so only reads started after the answer retire it. */
    await waitFor(() => expect(within(drawerElement()).queryAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(0));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  });

  /* #2068 item 10: a replayed send waits for a run read started after its answer. While that read fails, the composer
     stays closed, so the failure is said with a Try again instead of leaving nothing to press. */
  it('[#2068] offers a Try again while a replayed send waits on a run read that failed', async () => {
    const text = 'Replayed while the run read fails';
    let attempts = 0;
    let runFails = false;
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run') && runFails) return failure(503, 'unavailable', 'Run read unavailable');
      if (!request.path.endsWith('/planner/input')) return undefined;
      attempts += 1;
      if (attempts === 1) throw new Error('response dropped');
      runFails = true;
      return inputAccepted();
    });
    const runReads = () => requests.filter((request) => request.path.endsWith('/planner/run')).length;
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    await waitFor(() => expect(attempts).toBe(2));
    const alert = await within(drawerElement()).findByRole('alert');
    expect(alert.textContent).toContain('The conversation’s status could not be loaded.');
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    const before = runReads();
    runFails = false;
    fireEvent.click(within(drawerElement()).getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(runReads()).toBeGreaterThan(before));
    await waitFor(() => expect(within(drawerElement()).queryByRole('alert')).toBeNull());
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  });

  it('[#2068] keeps a composer draft when Try again resends a rejected message', async () => {
    let attempts = 0;
    const { requests } = setup((request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      attempts += 1;
      return attempts === 1 ? failure(429, 'rate_limited', 'Too many requests.') : undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('Rejected the first time');
    expect(within(await screen.findByRole('alert')).getByText('Not sent. Too many requests.')).toBeTruthy();
    await typeInto(messageField(), 'A draft in progress');
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(2));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    expect(messageField().textContent).toBe('A draft in progress');
  });

  it('[F5] leaves the footer as it was when Try again is pressed offline in the bundled build', async () => {
    vi.stubGlobal('__NC_BUNDLED__', true);
    const access = new RecoveryAccess(); access.change('connected');
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/input')) throw new Error('response dropped');
      return undefined;
    }, undefined, access);
    const inputs = () => requests.filter((request) => request.path.endsWith('/planner/input'));
    await sendUntilSpent('Spent online, retried offline');
    expect(inputs()).toHaveLength(SEND_RETRIES + 1);
    act(() => { access.change('offline'); });
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    /* The connection is the global recovery status's to speak of, not this op's. */
    expect(within(screen.getByRole('alert')).getByText('Delivery is unconfirmed.')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
    expect(inputs()).toHaveLength(SEND_RETRIES + 1);
  });

  it('[F5] says only that delivery is unconfirmed when the retries ran out offline (bundled build)', async () => {
    vi.stubGlobal('__NC_BUNDLED__', true);
    const access = new RecoveryAccess(); access.change('connected');
    const { requests } = setup((request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      access.change('offline');
      throw new Error('response dropped');
    }, undefined, access);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('Sent just before the connection went');
    expect(within(await screen.findByRole('alert')).getByText('Delivery is unconfirmed.')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
  });

  it('[F4] sends a dropped answer again under its key, with no question and one message', async () => {
    let attempts = 0;
    const text = 'Keep the dropped request';
    let rows: ReturnType<typeof harnessMessage>[] = [];
    const { requests } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) return ok(rows);
      if (!request.path.endsWith('/planner/input')) return undefined;
      attempts += 1;
      /* The first attempt was stored; only its answer was lost. */
      rows = [harnessMessage(1, 'userMessage', { content: [{ text }] })];
      if (attempts === 1) throw new Error('response dropped');
      return inputAccepted();
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write(text);
    await waitFor(() => expect(attempts).toBe(2));
    await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 0); }); });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.queryByRole('dialog')).toBeNull();
    const inputs = requests.filter((request) => request.path.endsWith('/planner/input'));
    expect(inputs.map((request) => request.body)).toEqual([{ text }, { text }]);
    expect(inputs[0]?.headers?.['Idempotency-Key']).toBeTruthy();
    expect(inputs[1]?.headers?.['Idempotency-Key']).toBe(inputs[0]?.headers?.['Idempotency-Key']);
    expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(1);
  });

  it('[F4] refuses a send pressed while offline in the bundled build, sends nothing and keeps the words', async () => {
    vi.stubGlobal('__NC_BUNDLED__', true);
    const access = new RecoveryAccess(); access.change('connected');
    const { requests } = setup(undefined, undefined, access);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    act(() => { access.change('offline'); });
    await write('Not while offline');
    await waitFor(() => expect(messageField().textContent).toBe('Not while offline'));
    /* No notice of its own: the global recovery status already says the workspace is offline. */
    expect(screen.queryByRole('alert')).toBeNull();
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(0);
  });

  /* A retry must carry the images the failed message was shown with; the defect
   * was in what the retry put on the wire. */
  const ATTACHMENT_ID = '0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png';

  function withAttachments(onInput: (attempt: number) => ApiTransportResponse | undefined) {
    let attempts = 0;
    return setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({
          card_id: pathCardId(request.path), worker_session_id: 'r', phase: 'idle', model: null, reasoning_effort: null,
          blocked_reason: null, attachments_supported: true, running_turn: null,
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

  /* #2043 step 5: every chat write reads its failure through its route's table. A lost answer gets a fixed state at
     the object and is never narrated there; the connection is the global recovery indicator's to speak of. */
  describe('[#2043] a chat write whose answer is lost', () => {
    const CONNECTIVITY = /Transport request failed|timed out|back online|连接恢复/;
    const timedOut = (): never => { throw new DOMException('Request timed out.', 'TimeoutError'); };
    const dropped = (): never => { throw new Error('response dropped'); };
    const chatText = () => drawerElement().textContent;
    const running = () => ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'turn_running', model: null,
      reasoning_effort: null, blocked_reason: null, attachments_supported: true, running_turn: null });
    async function openAssistant() {
      fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
      await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    }

    it('send: says only that delivery is unconfirmed', async () => {
      setup((request) => request.path.endsWith('/planner/input') ? timedOut() : undefined);
      await sendUntilSpent('Lost on the way');
      expect(within(screen.getByRole('alert')).getByText('Delivery is unconfirmed.')).toBeTruthy();
      expect(chatText()).not.toMatch(CONNECTIVITY);
    });

    it('Try again offline (bundled build): keeps the unconfirmed footer as it was', async () => {
      vi.stubGlobal('__NC_BUNDLED__', true);
      const access = new RecoveryAccess(); access.change('connected');
      setup((request) => request.path.endsWith('/planner/input') ? dropped() : undefined, undefined, access);
      await sendUntilSpent('Retried offline');
      act(() => { access.change('offline'); });
      fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
      expect(within(screen.getByRole('alert')).getByText('Delivery is unconfirmed.')).toBeTruthy();
      expect(chatText()).not.toMatch(CONNECTIVITY);
    });

    it('model select: says the change is unconfirmed', async () => {
      setup((request) => request.path.endsWith('/planner/model') ? timedOut() : undefined);
      await openAssistant();
      fireEvent.click(within(drawerElement()).getByRole('button', { name: /^Model:/ }));
      fireEvent.click(await screen.findByRole('menuitem', { name: /^Default/ }));
      expect((await screen.findByRole('alert')).textContent).toBe('The model change is unconfirmed.');
      expect(chatText()).not.toMatch(CONNECTIVITY);
    });

    it('Stop: shows Stop unconfirmed, never Stop failed', async () => {
      setup((request) => request.path.endsWith('/planner/run') ? running()
        : request.path.endsWith('/planner/interrupt') ? dropped() : undefined);
      fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
      fireEvent.click(await screen.findByRole('button', { name: 'Stop' }));
      fireEvent.click(await screen.findByRole('button', { name: 'Stop unconfirmed', expanded: false }));
      /* #2068 item 29: the detail also covers a stop request whose answer was lost and that may never have arrived. */
      expect(screen.getByText('The stop may not have taken effect: the response may still be running or may already have ended.',
        { exact: true })).toBeTruthy();
      expect(screen.queryByText('Stop failed')).toBeNull();
      expect(chatText()).not.toMatch(CONNECTIVITY);
    });

    it('image upload: says the image could not be uploaded', async () => {
      setup((request) => request.path.endsWith('/planner/run') ? ok({ ...running().body as object, phase: 'idle' })
        : request.path.endsWith('/planner/attachments') ? timedOut() : undefined);
      await openAssistant();
      await attachAnImage();
      expect(await within(drawerElement()).findByText('The image could not be uploaded.')).toBeTruthy();
      expect(chatText()).not.toMatch(CONNECTIVITY);
    });

    /* #2068 item 27: a write refused where it is admitted never left the browser, so it reads as not done, never as
       unconfirmed; why it could not go out is the global recovery indicator's to say. */
    describe('[#2068] refused at admission (bundled build, offline)', () => {
      async function offlineAfterOpening(reply: Reply) {
        vi.stubGlobal('__NC_BUNDLED__', true);
        const access = new RecoveryAccess(); access.change('connected');
        const { requests } = setup(reply, undefined, access);
        await openAssistant();
        act(() => { access.change('offline'); });
        return requests;
      }

      it('model select: says the model was not changed', async () => {
        const requests = await offlineAfterOpening(() => undefined);
        fireEvent.click(within(drawerElement()).getByRole('button', { name: /^Model:/ }));
        fireEvent.click(await screen.findByRole('menuitem', { name: /^Default/ }));
        expect((await screen.findByRole('alert')).textContent).toBe('The model was not changed.');
        expect(requests.filter((request) => request.path.endsWith('/planner/model'))).toEqual([]);
      });

      it('Stop: says the response was not stopped', async () => {
        const requests = await offlineAfterOpening((request) => request.path.endsWith('/planner/run') ? running() : undefined);
        fireEvent.click(await screen.findByRole('button', { name: 'Stop' }));
        fireEvent.click(await screen.findByRole('button', { name: 'Stop failed', expanded: false }));
        expect(screen.getByText('The response was not stopped.', { exact: true })).toBeTruthy();
        expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toEqual([]);
      });

      it('image upload: says the image was not uploaded', async () => {
        const requests = await offlineAfterOpening((request) => request.path.endsWith('/planner/run')
          ? ok({ ...running().body as object, phase: 'idle' }) : undefined);
        await attachAnImage();
        expect(await within(drawerElement()).findByText('The image was not uploaded.')).toBeTruthy();
        expect(requests.filter((request) => request.path.endsWith('/planner/attachments'))).toEqual([]);
      });
    });
  });

  it('[#1505] re-sends the image the failed message was shown with, not just its words', async () => {
    const text = 'look at this';
    const { requests } = withAttachments((attempt) => attempt === 1
      ? failure(429, 'rate_limited', 'Wait a moment')
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
      ? failure(429, 'rate_limited', 'Wait a moment')
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

  it('regenerates the original prompt and images once without clearing an unsent draft', async () => {
    const image = { id: ATTACHMENT_ID, contentType: 'image/png', size: 4,
      url: `/api/cards/${ASSISTANT_CARD.id}/planner/attachments/${ATTACHMENT_ID}` };
    const draftImageId = '0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e70.png';
    const user = { ...harnessMessage(91, 'userMessage', { content: [{ text: 'Original prompt' }] }),
      input_segments: [{ presentation: 'user', text: 'User says:\nOriginal prompt', attachments: [image] }] };
    const reply = harnessMessage(92, 'agentMessage', { text: 'Original answer' });
    const terminal = { ...harnessMessage(93, '', {}), item_type: null, turn_id: 'turn', method: 'turn/completed',
      params: JSON.stringify({ id: 'turn', status: 'completed', error: null }) };
    let resolve!: (response: ApiTransportResponse) => void;
    const held = new Promise<ApiTransportResponse>((done) => { resolve = done; });
    const { requests } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) return ok([user, reply, terminal]);
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'idle', model: null,
        reasoning_effort: null, blocked_reason: null, attachments_supported: true, running_turn: null });
      if (request.path.endsWith('/planner/attachments')) return ok({ attachmentId: draftImageId, contentType: 'image/png', size: 4,
        url: `/api/cards/${ASSISTANT_CARD.id}/planner/attachments/${draftImageId}` });
      if (request.path.endsWith('/planner/input')) return held;
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await screen.findByRole('button', { name: 'Regenerate response' });
    await typeInto(messageField(), 'Keep my separate draft');
    await attachAnImage();
    await waitFor(() => expect(Array.from(drawerElement().querySelectorAll('[data-nc-attachments] img')).some((image) => image.getAttribute('src')?.endsWith(draftImageId))).toBe(true));
    fireEvent.click(screen.getByRole('button', { name: 'Regenerate response' }));
    await waitFor(() => expect(inputBodies(requests)).toEqual([{ text: 'Original prompt', attachments: [ATTACHMENT_ID] }]));
    fireEvent.click(screen.getByRole('button', { name: /Regenerate response/ }));
    expect(inputBodies(requests)).toHaveLength(1);
    expect(messageField().textContent).toBe('Keep my separate draft');
    expect(screen.getByText('Original answer', { exact: true })).toBeTruthy();
    await act(async () => { resolve(inputAccepted()); await held; });
    expect(messageField().textContent).toBe('Keep my separate draft');
    expect(inputBodies(requests)).toHaveLength(1);
    expect(Array.from(drawerElement().querySelectorAll('[data-nc-attachments] img')).some((image) => image.getAttribute('src')?.endsWith(draftImageId))).toBe(true);
  });

  /* #2131 S7: Regenerate is a send. Its failure is the outbox's, read through SEND_FAILURES, never the action's own text. */
  it('regenerate whose answer is lost: says only that delivery is unconfirmed', async () => {
    const user = harnessMessage(91, 'userMessage', { content: [{ text: 'Original prompt' }] });
    const terminal = { ...harnessMessage(93, '', {}), item_type: null, turn_id: 'turn', method: 'turn/completed',
      params: JSON.stringify({ id: 'turn', status: 'completed', error: null }) };
    setup((request) => {
      if (request.path.includes(HISTORY_PATH)) return ok([user, harnessMessage(92, 'agentMessage', { text: 'Answer' }), terminal]);
      if (request.path.endsWith('/planner/input')) throw new Error('socket hang up');
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    fireEvent.click(await screen.findByRole('button', { name: 'Regenerate response' }));
    const alert = await screen.findByRole('alert');
    await waitFor(() => expect(within(alert).getByText('Delivery is unconfirmed.')).toBeTruthy());
    expect(drawerElement().textContent).not.toMatch(/Transport request failed|socket hang up|timed out|connection/i);
  });

  /* Edit (#1923): the turn's message comes back to the composer, and Send replaces the turn in one keyed request
     (#2043): `POST …/planner/input` naming `replaces_turn`, which the server answers as one commit. */
  const REWIND_IMAGE = { id: ATTACHMENT_ID, contentType: 'image/png', size: 4,
    url: `/api/cards/${ASSISTANT_CARD.id}/planner/attachments/${ATTACHMENT_ID}` };
  const REWIND_INPUT = [{ presentation: 'user', text: 'User says:\nOriginal prompt', attachments: [REWIND_IMAGE] }];
  /** Whether a request is a send that replaces a turn; the server removes the turn only when it answers 200. */
  const replacesTurn = (request: ApiRequest) => request.path.endsWith('/planner/input')
    && typeof (request.body as { replaces_turn?: unknown } | undefined)?.replaces_turn === 'string';
  const rewindRequests = (requests: readonly ApiRequest[]) => requests.filter((request) => request.path.endsWith('/planner/rewind'));
  const notReplaceable = () => failure(409, 'planner_turn_not_replaceable', 'This turn cannot be edited; nothing was changed');
  const composerImages = () => Array.from(drawerElement().querySelectorAll('[data-nc-attachments] img'))
    .map((image) => image.getAttribute('src'));

  /** One stored turn of the assistant conversation: its prompt (with images), reply and outcome. */
  const turnRows = (turnId: string, first: number, prompt: string, answer: string,
    images: readonly (typeof REWIND_IMAGE)[] = []) => [
    { ...harnessMessage(first, 'userMessage', { content: [{ text: prompt }] }), turn_id: turnId,
      input_segments: [{ presentation: 'user', text: `User says:\n${prompt}`, attachments: images }] },
    { ...harnessMessage(first + 1, 'agentMessage', { text: answer }), turn_id: turnId },
    { ...harnessMessage(first + 2, '', {}), item_type: null, turn_id: turnId, method: 'turn/completed',
      params: JSON.stringify({ id: turnId, status: 'completed', error: null }) },
  ];
  /** A conversation whose stored rows the case scripts read by read, answering run as an idle harness. */
  function scriptedSetup(rows: () => readonly unknown[], extra: Reply = () => undefined) {
    return setup(async (request) => {
      const answered = await extra(request);
      if (answered !== undefined) return answered;
      if (request.path.includes(HISTORY_PATH)) return ok(pathCardId(request.path) === ASSISTANT_CARD.id ? rows() : []);
      if (request.path.endsWith('/planner/run')) return ok({ card_id: pathCardId(request.path), worker_session_id: 'r',
        phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null, attachments_supported: true, running_turn: null });
      if (request.path.endsWith('/planner/attachments')) return ok({ attachmentId: DRAFT_IMAGE_ID, contentType: 'image/png', size: 4,
        url: `/api/cards/${pathCardId(request.path)}/planner/attachments/${DRAFT_IMAGE_ID}` });
      return undefined;
    });
  }
  const DRAFT_IMAGE_ID = '0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e70.png';

  /** The assistant conversation's one turn until a replace is answered 200, then nothing, as the server would page it. */
  function editSetup(input: () => ApiTransportResponse | undefined | Promise<ApiTransportResponse | undefined> = () => undefined,
    run: Record<string, unknown> = {}, uploadGate: () => Promise<void> = () => Promise.resolve()) {
    let removed = false;
    const user = { ...harnessMessage(91, 'userMessage', { content: [{ text: 'Original prompt' }] }), turn_id: 'turn',
      input_segments: REWIND_INPUT };
    const reply = { ...harnessMessage(92, 'agentMessage', { text: 'Original answer' }), turn_id: 'turn' };
    const terminal = { ...harnessMessage(93, '', {}), item_type: null, turn_id: 'turn', method: 'turn/completed',
      params: JSON.stringify({ id: 'turn', status: 'completed', error: null }) };
    return setup(async (request) => {
      if (request.path.includes(HISTORY_PATH)) {
        if (pathCardId(request.path) !== ASSISTANT_CARD.id) return ok([]);
        return ok(removed ? [] : [user, reply, terminal]);
      }
      if (request.path.endsWith('/planner/run')) {
        return ok({ card_id: pathCardId(request.path), worker_session_id: 'r', phase: 'idle', model: null,
          reasoning_effort: null, blocked_reason: null, attachments_supported: true, running_turn: null, ...run });
      }
      if (request.path.endsWith('/planner/attachments')) {
        await uploadGate();
        const draftImageId = '0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e70.png';
        return ok({ attachmentId: draftImageId, contentType: 'image/png', size: 4,
          url: `/api/cards/${pathCardId(request.path)}/planner/attachments/${draftImageId}` });
      }
      if (request.path.endsWith('/planner/input')) {
        const response = await input() ?? inputAccepted();
        if (response.status === 200 && replacesTurn(request)) removed = true;
        return response;
      }
      return undefined;
    });
  }

  async function openEditableAssistant() {
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await screen.findByRole('button', { name: 'Edit message' });
  }

  /** A send the case answers by hand, so what the press does is seen before any answer. */
  function heldAnswer() {
    let settle!: (response: ApiTransportResponse) => void;
    const held = new Promise<ApiTransportResponse>((done) => { settle = done; });
    return {
      reply: () => held,
      answer: async (response: ApiTransportResponse) => { await act(async () => { settle(response); await held; }); },
    };
  }
  const settleFor = (ms: number) => act(async () => { await new Promise((done) => setTimeout(done, ms)); });
  const editBar = () => drawerElement().querySelector<HTMLElement>('[data-nc-edit-bar]');
  const markedMessages = () => Array.from(drawerElement().querySelectorAll('[data-nc-turn="you"][data-nc-editing]')).map((said) => said.textContent);
  const markedImages = () => Array.from(drawerElement().querySelectorAll('[data-nc-turn-attachments][data-nc-editing] img')).map((image) => image.getAttribute('src'));
  const REFUSED_NOTE = 'Your message is back in the composer; sending adds a new one.';
  /** An earlier turn, then the one to edit (with its image), until a replace is answered 200. */
  function twoTurnSetup(input: () => ApiTransportResponse | Promise<ApiTransportResponse> = inputAccepted) {
    let stage: 'before' | 'replaced' = 'before';
    const earlier = turnRows('turn-0', 81, 'Earlier prompt', 'Earlier answer');
    return scriptedSetup(() => stage === 'before'
      ? [...earlier, ...turnRows('turn', 91, 'Original prompt', 'Original answer', [REWIND_IMAGE])] : earlier,
    async (request) => {
      if (!request.path.endsWith('/planner/input')) return undefined;
      const response = await input();
      if (response.status === 200 && replacesTurn(request)) stage = 'replaced';
      return response;
    });
  }

  it('enters edit mode at the click without asking the server; the turn stays, marked, and nothing acts on it', async () => {
    const { requests } = twoTurnSetup();
    await openEditableAssistant();
    const before = requests.length;
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    expect(messageField().textContent).toBe('Original prompt');
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
    expect(editBar()?.textContent).toContain('Editing message');
    expect(editBar()?.textContent).toContain('Original prompt');
    expect(screen.getByText('Original answer', { exact: true })).toBeTruthy();
    expect(markedMessages()).toEqual(['Original prompt']);
    expect(markedImages()).toEqual([REWIND_IMAGE.url]);
    expect(screen.getByRole('button', { name: 'Replace message' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Send' })).toBeNull();
    expect(messageField().getAttribute('contenteditable')).toBe('true');
    await waitFor(() => expect(document.activeElement).toBe(messageField()));
    for (const name of ['Edit message (not available now)', 'Regenerate response (not available now)', 'Copy response (not available yet)']) {
      fireEvent.click(screen.getByRole('button', { name }));
    }
    await settleFor(20);
    expect(requests.slice(before).filter((request) => request.method === 'POST')).toEqual([]);
  });

  it.each([
    ['✕ empties an untouched composer', false, () => fireEvent.click(screen.getByRole('button', { name: 'Cancel edit' }))],
    ['Esc empties an untouched composer', false, () => fireEvent.keyDown(messageField(), { key: 'Escape' })],
    ['✕ keeps what the reader changed', true, () => fireEvent.click(screen.getByRole('button', { name: 'Cancel edit' }))],
  ] as const)('leaves edit mode without a request: %s', async (_, change, cancel) => {
    const { requests } = twoTurnSetup();
    await openEditableAssistant();
    const before = requests.length;
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    if (change) await typeInto(messageField(), 'Revised prompt');
    cancel();
    expect(editBar()).toBeNull();
    expect(markedMessages()).toEqual([]);
    expect(markedImages()).toEqual([]);
    expect(screen.getByRole('complementary', { name: /^(Assistant|Earlier prompt)$/ })).toBeTruthy();
    if (change) {
      expect(messageField().textContent).toBe('Revised prompt');
      expect(composerImages()).toEqual([REWIND_IMAGE.url]);
    } else {
      await waitFor(() => expect(messageField().textContent).toBe(''));
      expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
      expect(screen.getByRole('button', { name: 'Edit message' })).toBeTruthy();
    }
    await settleFor(20);
    expect(requests.slice(before).filter((request) => request.method === 'POST')).toEqual([]);
  });

  it('replaces the message on Send with one keyed request: a spinner, the turn kept and marked until its answer', async () => {
    const answer = heldAnswer();
    const { requests } = twoTurnSetup(answer.reply);
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await typeInto(messageField(), 'Revised prompt');
    await submit();
    await waitFor(() => expect(inputBodies(requests))
      .toEqual([{ text: 'Revised prompt', attachments: [ATTACHMENT_ID], replaces_turn: 'turn' }]));
    expect(requests.find(replacesTurn)?.headers?.['Idempotency-Key']).toMatch(/.+/);
    expect(rewindRequests(requests)).toEqual([]);
    expect(screen.getByRole('button', { name: 'Sending…' }).hasAttribute('disabled')).toBe(true);
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    /* Only the server knows whether the turn can go: it stays, marked, and nothing pretends the new message is in. */
    expect(screen.getByText('Original answer', { exact: true })).toBeTruthy();
    expect(markedMessages()).toEqual(['Original prompt']);
    expect(screen.queryByText('Revised prompt', { exact: true })).toBeNull();
    /* The words live in the send now: no edit bar, so no ✕ can drop them (#2041). */
    expect(editBar()).toBeNull();
    expect(screen.queryByRole('button', { name: 'Cancel edit' })).toBeNull();
    await answer.answer(inputAccepted());
    await waitFor(() => expect(screen.queryByText('Original answer', { exact: true })).toBeNull());
    expect(screen.getByText('Revised prompt', { exact: true })).toBeTruthy();
    await settleFor(20);
    expect(inputBodies(requests)).toHaveLength(1);
    expect(rewindRequests(requests)).toEqual([]);
    expect(screen.queryByRole('button', { name: 'Sending…' })).toBeNull();
  });

  it('sends outside edit mode with no turn to replace', async () => {
    const { requests } = twoTurnSetup();
    await openEditableAssistant();
    await typeInto(messageField(), 'A new message');
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toEqual([{ text: 'A new message' }]));
    await settleFor(20);
    expect(screen.getByText('Original answer', { exact: true })).toBeTruthy();
  });

  it('sends a replace whose answer was lost again under the same key, and hides the turn only on its 200', async () => {
    let attempts = 0;
    const { requests } = twoTurnSetup(() => {
      attempts += 1;
      return attempts === 1 ? failure(502, 'bad_gateway', 'Upstream unavailable') : inputAccepted();
    });
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(2));
    const sent = requests.filter(replacesTurn);
    expect(sent.map((request) => request.body)).toEqual([
      { text: 'Original prompt', attachments: [ATTACHMENT_ID], replaces_turn: 'turn' },
      { text: 'Original prompt', attachments: [ATTACHMENT_ID], replaces_turn: 'turn' },
    ]);
    expect(sent[1].headers?.['Idempotency-Key']).toBe(sent[0].headers?.['Idempotency-Key']);
    await waitFor(() => expect(screen.queryByText('Original answer', { exact: true })).toBeNull());
    expect(screen.getByText('Earlier answer', { exact: true })).toBeTruthy();
    expect(rewindRequests(requests)).toEqual([]);
  });

  it('never removes the turn when the replace is refused: edit mode ends, the composer keeps the message, a notice says why', async () => {
    const { requests } = twoTurnSetup(notReplaceable);
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await submit();
    await screen.findByText('Edit failed: This turn cannot be edited; nothing was changed');
    expect(screen.getByText(REFUSED_NOTE)).toBeTruthy();
    /* One line for the refusal: the send footer does not repeat it. */
    expect(screen.queryByText(/^Not sent\./)).toBeNull();
    expect(editBar()).toBeNull();
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
    expect(screen.getByText('Original answer', { exact: true })).toBeTruthy();
    expect(markedMessages()).toEqual([]);
    await settleFor(20);
    expect(inputBodies(requests)).toEqual([{ text: 'Original prompt', attachments: [ATTACHMENT_ID], replaces_turn: 'turn' }]);
    /* Cleared by the next send, which is an ordinary new message. */
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(2));
    expect(inputBodies(requests)[1]).toEqual({ text: 'Original prompt', attachments: [ATTACHMENT_ID] });
    expect(screen.queryByText('Edit failed: This turn cannot be edited; nothing was changed')).toBeNull();
  });

  it('leaves nothing of a refused replace but its words: emptied, the thread is the server’s and the turn editable again', async () => {
    const { requests } = twoTurnSetup(notReplaceable);
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await submit();
    await screen.findByText('Edit failed: This turn cannot be edited; nothing was changed');
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    await clearField();
    removeComposerImage();
    await waitFor(() => expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull());
    await settleFor(20);
    /* No echo of the refused send: the thread is the two stored turns, and the edited one is still the latest. */
    expect(Array.from(drawerElement().querySelectorAll('[data-nc-turn="you"]')).map((said) => said.textContent))
      .toEqual(['Earlier prompt', 'Original prompt']);
    expect(screen.getByText('Original answer', { exact: true })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Edit message' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Regenerate response' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Copy response' })).toBeTruthy();
    /* The notice no longer says the words are in the composer, which they are not. */
    expect(screen.getByText('Edit failed: This turn cannot be edited; nothing was changed')).toBeTruthy();
    expect(screen.queryByText(REFUSED_NOTE)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(inputBodies(requests)).toHaveLength(1);
    /* And it can be edited again, which clears the notice. */
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    expect(messageField().textContent).toBe('Original prompt');
    expect(screen.queryByText('Edit failed: This turn cannot be edited; nothing was changed')).toBeNull();
  });

  it('[#2041] keeps a replace whose answer stays lost as an unconfirmed send: Try again replays it under its key', async () => {
    let lost = true;
    const { requests } = twoTurnSetup(() => lost ? failure(502, 'bad_gateway', 'Upstream unavailable') : inputAccepted());
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await submit();
    expect((await screen.findByRole('alert')).textContent).toContain('Delivery is unconfirmed');
    expect(inputBodies(requests)).toHaveLength(SEND_RETRIES + 1);
    /* The only copy of the words is the unconfirmed send: no edit mode, no ✕, and the composer is not refilled. */
    expect(editBar()).toBeNull();
    expect(messageField().textContent).toBe('');
    expect(screen.getByText('Original answer', { exact: true })).toBeTruthy();
    const key = requests.filter(replacesTurn)[0].headers?.['Idempotency-Key'];
    lost = false;
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(SEND_RETRIES + 2));
    const retried = requests.filter(replacesTurn).at(-1);
    expect(retried?.body).toEqual({ text: 'Original prompt', attachments: [ATTACHMENT_ID], replaces_turn: 'turn' });
    expect(retried?.headers?.['Idempotency-Key']).toBe(key);
    await waitFor(() => expect(screen.queryByText('Original answer', { exact: true })).toBeNull());
    expect(rewindRequests(requests)).toEqual([]);
  });

  it('keeps the composer when edit mode is cancelled while an image is still uploading', async () => {
    let finish!: () => void;
    const finished = new Promise<void>((done) => { finish = done; });
    editSetup(undefined, {}, () => finished);
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await attachAnImage();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel edit' }));
    expect(editBar()).toBeNull();
    expect(messageField().textContent).toBe('Original prompt');
    await act(async () => { finish(); await finished; });
    await waitFor(() => expect(composerImages().map((src) => src?.split('/').pop())).toEqual([ATTACHMENT_ID, DRAFT_IMAGE_ID]));
    expect(messageField().textContent).toBe('Original prompt');
  });

  it('leaves edit mode, keeping the composer, once a newer turn arrives', async () => {
    let newer = false;
    const earlier = turnRows('turn', 91, 'Original prompt', 'Original answer', [REWIND_IMAGE]);
    const { client } = scriptedSetup(() => newer ? [...earlier, ...turnRows('turn-2', 101, 'From elsewhere', 'Newer answer')] : earlier);
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await typeInto(messageField(), 'Revised prompt');
    newer = true;
    await act(async () => { await client.invalidateQueries(); });
    await screen.findByText('Newer answer', { exact: true });
    await screen.findByText('This message can no longer be replaced; sending adds a new one.');
    expect(editBar()).toBeNull();
    expect(messageField().textContent).toBe('Revised prompt');
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
  });

  it('keeps edit mode with its conversation across a switch, touching nothing in the other one', async () => {
    editSetup();
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await pickPlanner();
    await waitFor(() => expect(messageField().textContent).toBe(''));
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
    expect(editBar()).toBeNull();
    expect(screen.getByRole('button', { name: 'Send' })).toBeTruthy();
    await pickAssistant();
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
    expect(editBar()?.textContent).toContain('Original prompt');
  });

  it('keeps edit mode when its conversation is shown again after another one sent a message', async () => {
    editSetup();
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await pickPlanner();
    await typeInto(messageField(), 'Planner words');
    await submit();
    await pickAssistant();
    await settleFor(50);
    expect(editBar()?.textContent).toContain('Original prompt');
    expect(screen.queryByText('This message can no longer be replaced; sending adds a new one.')).toBeNull();
  });

  it('never carries a delivered message the server has not shown yet into the next conversation', async () => {
    editSetup();
    await openEditableAssistant();
    await typeInto(messageField(), 'Delivered words');
    await submit();
    await settleFor(50);
    await pickPlanner();
    await settleFor(50);
    expect(drawerElement().querySelector('[data-nc-turn="you"]')).toBeNull();
    expect(messageField().getAttribute('contenteditable')).toBe('true');
  });

  it('sends a replacement pressed before a switch to its own conversation, and draws nothing in the one shown', async () => {
    const answer = heldAnswer();
    const { requests } = editSetup(answer.reply);
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await submit();
    await pickPlanner();
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    expect(screen.queryByRole('button', { name: 'Sending…' })).toBeNull();
    await answer.answer(inputAccepted());
    await waitFor(() => expect(requests.filter((request) => request.path.endsWith('/planner/input'))
      .map((request) => [pathCardId(request.path), request.body]))
      .toEqual([[ASSISTANT_CARD.id, { text: 'Original prompt', attachments: [ATTACHMENT_ID], replaces_turn: 'turn' }]]));
    await settleFor(20);
    expect(messageField().getAttribute('contenteditable')).toBe('true');
    expect(drawerElement().querySelector('[data-nc-turn="you"]')).toBeNull();
    expect(messageField().textContent).toBe('');
  });

  const pickAssistant = async () => {
    fireEvent.click(screen.getByRole('button', { name: /^Conversation (Assistant|Original prompt)/ }));
    await screen.findByRole('complementary', { name: /^(Assistant|Original prompt)$/ });
  };
  const reopenAssistant = async () => {
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    await pickAssistant();
  };
  const clearField = async () => {
    messageField().textContent = '';
    await act(async () => { fireEvent.input(messageField()); await Promise.resolve(); });
  };
  async function editIntoComposer(requests: readonly ApiRequest[]) {
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    expect(messageField().textContent).toBe('Original prompt');
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
    expect(inputBodies(requests)).toEqual([]);
  }
  const removeComposerImage = () => fireEvent.click(within(drawerElement().querySelector<HTMLElement>('[data-nc-attachments]')!)
    .getByRole('button', { name: /remove/i }));

  it('keeps the edited message through closing and reopening its conversation', async () => {
    const { requests } = editSetup();
    await editIntoComposer(requests);
    await reopenAssistant();
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
  });

  it('carries the reader’s changes to the edited message, and never the message itself, across a switch', async () => {
    const { requests } = editSetup();
    await editIntoComposer(requests);
    await typeInto(messageField(), 'Revised prompt');
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Planner chat' }));
    await screen.findByRole('complementary', { name: 'Planner chat' });
    await waitFor(() => expect(messageField().textContent).toBe(''));
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
    await pickAssistant();
    await waitFor(() => expect(messageField().textContent).toBe('Revised prompt'));
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
  });

  const pickPlanner = async () => {
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Planner chat' }));
    await screen.findByRole('complementary', { name: 'Planner chat' });
  };
  const DRAFT_IMAGE_SUFFIX = '0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e70.png';

  it('gives each conversation its own composer: nothing carries across a switch, both survive it', async () => {
    const { requests } = editSetup();
    await editIntoComposer(requests);
    await pickPlanner();
    await waitFor(() => expect(messageField().textContent).toBe(''));
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
    await typeInto(messageField(), 'Words for the planner');
    await pickAssistant();
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
    await pickPlanner();
    await waitFor(() => expect(messageField().textContent).toBe('Words for the planner'));
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
  });

  it('keeps a typed draft through closing and reopening its conversation', async () => {
    editSetup();
    await openEditableAssistant();
    await typeInto(messageField(), 'Half a thought');
    await reopenAssistant();
    await waitFor(() => expect(messageField().textContent).toBe('Half a thought'));
    /* The draft withholds Edit: nothing is ever merged into it. */
    expect(screen.getByRole('button', { name: 'Edit message (not available now)' })).toBeTruthy();
  });

  it('does not bring an edited message back after its failed send is tried again', async () => {
    let attempts = 0;
    const { requests } = editSetup(() => {
      attempts += 1;
      return attempts === 1 ? failure(429, 'rate_limited', 'Not this time') : undefined;
    });
    await editIntoComposer(requests);
    await submit();
    fireEvent.click(await screen.findByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(2));
    expect(inputBodies(requests)[1]).toEqual({ text: 'Original prompt', attachments: [ATTACHMENT_ID], replaces_turn: 'turn' });
    await reopenAssistant();
    await act(async () => { await Promise.resolve(); });
    expect(messageField().textContent).toBe('');
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
  });

  it('does not bring an edited message back when the reader switches away before its send is answered', async () => {
    let answer!: () => void;
    const answered = new Promise<void>((done) => { answer = done; });
    const { requests } = editSetup(async () => { await answered; return undefined; });
    await editIntoComposer(requests);
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(1));
    await pickPlanner();
    await act(async () => { answer(); await answered; });
    await typeInto(messageField(), 'Planner words while A sends');
    await pickAssistant();
    await act(async () => { await Promise.resolve(); });
    expect(messageField().textContent).toBe('');
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
    await pickPlanner();
    await waitFor(() => expect(messageField().textContent).toBe('Planner words while A sends'));
  });

  it('lets another conversation be written while one conversation’s send is out', async () => {
    let answer!: () => void;
    const answered = new Promise<void>((done) => { answer = done; });
    const { requests } = editSetup(async () => { await answered; return undefined; });
    await openEditableAssistant();
    await typeInto(messageField(), 'Sent from the assistant');
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(1));
    await pickPlanner();
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await typeInto(messageField(), 'First words');
    await typeInto(messageField(), 'Second words');
    await pickAssistant();
    await pickPlanner();
    await waitFor(() => expect(messageField().textContent).toBe('Second words'));
    await act(async () => { answer(); await answered; });
  });

  it('[#2041] leaves the next conversation’s queue free while the last one’s delete is out', async () => {
    let answer!: () => void;
    const answered = new Promise<void>((done) => { answer = done; });
    setup(async (request) => {
      const card = pathCardId(request.path);
      if (request.path.endsWith('/planner/run')) {
        return ok({ card_id: card, worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null,
          running_turn: null, pending: [{ entry_id: `entry-${card}`, text: `queued in ${card}`, rev: 1, queued_at_ms: 5 }], pending_overflow: 0 });
      }
      if (request.method === 'DELETE' && request.path.includes('/planner/input/')) {
        await answered;
        return ok({ card_id: card, entry_id: `entry-${card}`, rev: 2, text: null });
      }
      return undefined;
    });
    const entryOf = (card: string) => Array.from(document.querySelectorAll<HTMLElement>('[data-nc-pending-entry]'))
      .find((entry) => entry.dataset.ncPendingEntry === `entry-${card}`);
    const deleteIn = (card: string) => within(entryOf(card)!).getByRole('button', { name: 'Delete this message' });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await waitFor(() => expect(entryOf(ASSISTANT_CARD.id)).toBeDefined());
    fireEvent.click(deleteIn(ASSISTANT_CARD.id));
    await waitFor(() => expect(deleteIn(ASSISTANT_CARD.id).hasAttribute('disabled')).toBe(true));
    await pickPlanner();
    await waitFor(() => expect(entryOf(PLANNER_CARD.id)).toBeDefined());
    expect(deleteIn(PLANNER_CARD.id).hasAttribute('disabled')).toBe(false);
    await act(async () => { answer(); await answered; });
  });

  /* #2068 item 15: the queue strip remounts on the way back, and the delete still out is its conversation's, not the
     strip's: the entry it is deleting offers no second delete until it is answered. */
  it('[#2068] keeps a conversation’s queue locked across a round trip while its delete is out', async () => {
    let answer!: () => void;
    const answered = new Promise<void>((done) => { answer = done; });
    const { requests } = setup(async (request) => {
      const card = pathCardId(request.path);
      if (request.path.endsWith('/planner/run')) {
        return ok({ card_id: card, worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null,
          running_turn: null, pending: [{ entry_id: `entry-${card}`, text: `queued in ${card}`, rev: 1, queued_at_ms: 5 }], pending_overflow: 0 });
      }
      if (request.method === 'DELETE' && request.path.includes('/planner/input/')) {
        await answered;
        return ok({ card_id: card, entry_id: `entry-${card}`, rev: 2, text: null });
      }
      return undefined;
    });
    const deletes = () => requests.filter((request) => request.method === 'DELETE');
    const entryOf = (card: string) => Array.from(document.querySelectorAll<HTMLElement>('[data-nc-pending-entry]'))
      .find((entry) => entry.dataset.ncPendingEntry === `entry-${card}`);
    const deleteIn = (card: string) => within(entryOf(card)!).getByRole('button', { name: 'Delete this message' });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await waitFor(() => expect(entryOf(ASSISTANT_CARD.id)).toBeDefined());
    fireEvent.click(deleteIn(ASSISTANT_CARD.id));
    await waitFor(() => expect(deletes()).toHaveLength(1));
    await pickPlanner();
    await waitFor(() => expect(entryOf(PLANNER_CARD.id)).toBeDefined());
    await pickAssistant();
    await waitFor(() => expect(entryOf(ASSISTANT_CARD.id)).toBeDefined());
    expect(deleteIn(ASSISTANT_CARD.id).hasAttribute('disabled')).toBe(true);
    fireEvent.click(deleteIn(ASSISTANT_CARD.id));
    await act(async () => { answer(); await answered; });
    expect(deletes()).toHaveLength(1);
  });

  it('puts an image whose upload finishes after a switch into the conversation it was picked in', async () => {
    let finish!: () => void;
    const finished = new Promise<void>((done) => { finish = done; });
    editSetup(undefined, {}, () => finished);
    await openEditableAssistant();
    await attachAnImage();
    await pickPlanner();
    await act(async () => { finish(); await finished; });
    await act(async () => { await Promise.resolve(); });
    expect(drawerElement().querySelector('[data-nc-attachments] img')).toBeNull();
    await pickAssistant();
    await waitFor(() => expect(composerImages().some((src) => src?.endsWith(DRAFT_IMAGE_SUFFIX))).toBe(true));
  });

  it('keeps the images of a rejected send for the footer’s Edit', async () => {
    const { requests } = editSetup(() => failure(429, 'rate_limited', 'Not this time'));
    await editIntoComposer(requests);
    await submit();
    fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
  });

  it('keeps the images of a refused send in the composer', async () => {
    const { requests } = editSetup(() => failure(409, 'planner_harness_dormant', 'No live session.'));
    await editIntoComposer(requests);
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(1));
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
  });

  it('clears a delivered message’s images from the conversation it was sent from, not the one shown', async () => {
    let answer!: () => void;
    const answered = new Promise<void>((done) => { answer = done; });
    const { requests } = editSetup(async () => { await answered; return undefined; });
    await editIntoComposer(requests);
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(1));
    await pickPlanner();
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await attachAnImage();
    await waitFor(() => expect(composerImages().some((src) => src?.endsWith(DRAFT_IMAGE_SUFFIX))).toBe(true));
    await act(async () => { answer(); await answered; });
    await act(async () => { await Promise.resolve(); });
    expect(composerImages().some((src) => src?.endsWith(DRAFT_IMAGE_SUFFIX))).toBe(true);
    await pickAssistant();
    await act(async () => { await Promise.resolve(); });
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
  });

  it('keeps a replaced turn hidden, and every action withheld, until a transcript read without it lands', async () => {
    let stage: 'before' | 'replaced' = 'before';
    let failReads = true;
    const earlier = turnRows('turn-0', 81, 'Earlier prompt', 'Earlier answer');
    const { requests } = setup((request) => {
      if (request.path.includes(HISTORY_PATH) && pathCardId(request.path) === ASSISTANT_CARD.id) {
        if (stage === 'before') return ok([...earlier, ...turnRows('turn', 91, 'Original prompt', 'Original answer')]);
        if (failReads) return failure(503, 'unavailable', 'Transcript unavailable');
        return ok([...earlier, ...turnRows('turn-2', 101, 'Original prompt', 'Replacement answer')]);
      }
      if (request.path.endsWith('/planner/run')) return ok({ card_id: pathCardId(request.path), worker_session_id: 'r',
        phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null, attachments_supported: true, running_turn: null });
      if (request.path.endsWith('/planner/input')) { stage = 'replaced'; return inputAccepted(); }
      return undefined;
    });
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toEqual([{ text: 'Original prompt', replaces_turn: 'turn' }]));
    /* The re-read failed: the cached transcript still holds the replaced turn, which stays hidden. */
    await screen.findByRole('button', { name: 'Try again' });
    expect(screen.queryByText('Original answer', { exact: true })).toBeNull();
    expect(screen.getByText('Earlier answer', { exact: true })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Copy response' })).toBeNull();
    failReads = false;
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await screen.findByText('Replacement answer', { exact: true });
    expect(screen.queryByText('Original answer', { exact: true })).toBeNull();
    expect(await screen.findByRole('button', { name: 'Copy response' })).toBeTruthy();
  });

  it('keeps an upload in flight busy across a remount, offering no Edit until it lands', async () => {
    let finish!: () => void;
    const finished = new Promise<void>((done) => { finish = done; });
    const { router } = editSetup(undefined, {}, () => finished);
    await openEditableAssistant();
    await attachAnImage();
    await waitFor(() => expect(screen.getByRole('button', { name: 'Edit message (not available now)' })).toBeTruthy());
    await act(() => router.navigate({ to: '/track/w2' }));
    await act(() => router.navigate({ to: '/track/w1' }));
    await screen.findByRole('complementary', { name: /^(Assistant|Original prompt)$/ });
    await screen.findByText('Original answer', { exact: true });
    await act(async () => { await new Promise((done) => setTimeout(done, 20)); });
    expect(screen.getByRole('button', { name: 'Edit message (not available now)' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Edit message' })).toBeNull();
    await act(async () => { finish(); await finished; });
    await waitFor(() => expect(composerImages().some((src) => src?.endsWith(DRAFT_IMAGE_SUFFIX))).toBe(true));
  });

  it('leaves the composer’s images alone when a delivered Regenerate sends the same images', async () => {
    const { requests } = editSetup();
    await editIntoComposer(requests);
    await typeInto(messageField(), 'Revised prompt');
    fireEvent.click(screen.getByRole('button', { name: 'Cancel edit' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Regenerate response' }));
    await waitFor(() => expect(inputBodies(requests)).toEqual([{ text: 'Original prompt', attachments: [ATTACHMENT_ID] }]));
    await act(async () => { await new Promise((done) => setTimeout(done, 20)); });
    expect(messageField().textContent).toBe('Revised prompt');
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
  });

  it('puts a failed Regenerate’s words and images back beside the composer’s own image', async () => {
    const { requests } = editSetup(() => failure(429, 'rate_limited', 'Not this time'));
    await openEditableAssistant();
    await attachAnImage();
    await waitFor(() => expect(composerImages().some((src) => src?.endsWith(DRAFT_IMAGE_ID))).toBe(true));
    fireEvent.click(screen.getByRole('button', { name: 'Regenerate response' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    expect(composerImages().map((src) => src?.split('/').pop())).toEqual([DRAFT_IMAGE_ID, ATTACHMENT_ID]);
    expect(inputBodies(requests)).toHaveLength(1);
  });

  it('puts a failed send’s words back with its image once', async () => {
    editSetup(() => failure(429, 'rate_limited', 'Not this time'));
    await openEditableAssistant();
    await attachAnImage();
    await waitFor(() => expect(composerImages().some((src) => src?.endsWith(DRAFT_IMAGE_ID))).toBe(true));
    await typeInto(messageField(), 'Look at this');
    await submit();
    fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    await waitFor(() => expect(messageField().textContent).toBe('Look at this'));
    expect(composerImages().map((src) => src?.split('/').pop())).toEqual([DRAFT_IMAGE_ID]);
  });

  it('acts again on a later turn that reuses the removed turn’s id', async () => {
    let stage: 'before' | 'replaced' = 'before';
    const { requests } = scriptedSetup(() => stage === 'before' ? turnRows('turn', 91, 'Original prompt', 'Original answer', [REWIND_IMAGE])
      : turnRows('turn', 101, 'Original prompt', 'A new answer', [REWIND_IMAGE]), (request) => {
      if (request.path.endsWith('/planner/input')) { stage = 'replaced'; return inputAccepted(); }
      return undefined;
    });
    await editIntoComposer(requests);
    await submit();
    await screen.findByText('A new answer', { exact: true });
    expect(await screen.findByRole('button', { name: 'Regenerate response' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Copy response' })).toBeTruthy();
  });

  it('retires the edited message once it is delivered', async () => {
    const { requests } = editSetup();
    await editIntoComposer(requests);
    await submit();
    await waitFor(() => expect(inputBodies(requests))
      .toEqual([{ text: 'Original prompt', attachments: [ATTACHMENT_ID], replaces_turn: 'turn' }]));
    await waitFor(() => expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull());
    await reopenAssistant();
    await act(async () => { await Promise.resolve(); });
    expect(messageField().textContent).toBe('');
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
  });

  it('keeps a text-only edited message whose send is refused', async () => {
    const { requests } = scriptedSetup(() => turnRows('turn', 91, 'Words only', 'Original answer'), (request) => {
      if (request.path.endsWith('/planner/input')) return failure(409, 'planner_harness_dormant', 'No live session.');
      return undefined;
    });
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    expect(messageField().textContent).toBe('Words only');
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toEqual([{ text: 'Words only', replaces_turn: 'turn' }]));
    await waitFor(() => expect(messageField().textContent).toBe('Words only'));
    /* Refused, so the turn stays and the row is still named after it. */
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: /^Conversation (Assistant|Words only)/ }));
    await waitFor(() => expect(messageField().textContent).toBe('Words only'));
  });

  it('retires the edited message when the reader empties the composer', async () => {
    const { requests } = editSetup();
    await editIntoComposer(requests);
    await clearField();
    removeComposerImage();
    await waitFor(() => expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull());
    await reopenAssistant();
    await act(async () => { await Promise.resolve(); });
    expect(messageField().textContent).toBe('');
    expect(drawerElement().querySelector('[data-nc-attachments]')).toBeNull();
    expect(inputBodies(requests)).toEqual([]);
  });

  /* #2068 item 19: an answer that refuses the body itself can never be sent as it is, so it is settled as a refusal:
     its words and images go back to the composer with the server's reason, and nothing offers a Try again. */
  it.each([400, 403, 404, 413, 422])('[#2068] gives a send refused %s back to the composer with its reason, offering no Try again', async (status) => {
    const text = 'look at these';
    const { requests } = withAttachments(() => failure(status, 'bad_request', 'The server will never take this.'));
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await attachAnImage();
    await write(text);
    expect((await screen.findByRole('alert')).textContent).toBe('Not sent. The server will never take this.');
    await waitFor(() => expect(messageField().textContent).toBe(text));
    expect(composerImages()).toEqual([REWIND_IMAGE.url]);
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
    /* The words are the composer's alone: no copy of a message the server never took is drawn. */
    expect(drawerElement().querySelector('[data-nc-turn="you"]')).toBeNull();
    expect(inputBodies(requests)).toEqual([{ text, attachments: [ATTACHMENT_ID] }]);
  });

  it('[#2068] puts at most the images a message carries in the composer for an Edit, saying the rest were not attached', async () => {
    const images = Array.from({ length: MAX_ATTACHMENTS_PER_MESSAGE + 1 }, (_, index) => ({
      ...REWIND_IMAGE, id: `image-${index}.png`, url: `/api/cards/${ASSISTANT_CARD.id}/planner/attachments/image-${index}.png`,
    }));
    const { requests } = scriptedSetup(() => turnRows('turn', 91, 'Original prompt', 'Original answer', images));
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    expect(composerImages()).toEqual(images.slice(0, MAX_ATTACHMENTS_PER_MESSAGE).map((image) => image.url));
    expect(within(drawerElement()).getByText('That image was not attached').parentElement?.textContent)
      .toContain(`A message can carry at most ${MAX_ATTACHMENTS_PER_MESSAGE} images.`);
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(1));
    expect(inputBodies(requests)[0]).toEqual({ text: 'Original prompt', replaces_turn: 'turn',
      attachments: images.slice(0, MAX_ATTACHMENTS_PER_MESSAGE).map((image) => image.id) });
  });

  /* #2068 item 31: a refusal's images are added to what the composer already holds, and the cap is the merge's: the
     images it could not take are said, never dropped silently. */
  it('[#2068] says which images a refused Regenerate could not put back beside the composer’s own', async () => {
    const images = Array.from({ length: MAX_ATTACHMENTS_PER_MESSAGE }, (_, index) => ({
      ...REWIND_IMAGE, id: `image-${index}.png`, url: `/api/cards/${ASSISTANT_CARD.id}/planner/attachments/image-${index}.png`,
    }));
    const { requests } = scriptedSetup(() => turnRows('turn', 91, 'Original prompt', 'Original answer', images), (request) =>
      request.path.endsWith('/planner/input') ? failure(400, 'bad_request', 'The server will never take this.') : undefined);
    await openEditableAssistant();
    await attachAnImage();
    await waitFor(() => expect(composerImages().map((src) => src?.split('/').pop())).toEqual([DRAFT_IMAGE_ID]));
    fireEvent.click(screen.getByRole('button', { name: 'Regenerate response' }));
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(1));
    await waitFor(() => expect(messageField().textContent).toBe('Original prompt'));
    expect(composerImages().map((src) => src?.split('/').pop()))
      .toEqual([DRAFT_IMAGE_ID, ...images.slice(0, MAX_ATTACHMENTS_PER_MESSAGE - 1).map((image) => image.id)]);
    expect(within(drawerElement()).getByText('That image was not attached').parentElement?.textContent)
      .toContain(`A message can carry at most ${MAX_ATTACHMENTS_PER_MESSAGE} images.`);
  });

  /* #2068 items 13 and 22: a refusal gives the words back through the registry, to the composer of the conversation
     the send was pressed in, whichever conversation is shown when it lands. */
  it('[#2068] gives a refused send its words back in its own conversation when the refusal lands while another is shown', async () => {
    let release!: () => void;
    const held = new Promise<void>((done) => { release = done; });
    const { requests } = editSetup(async () => {
      await held;
      return failure(409, 'planner_harness_runtime_superseded', 'Your message was not stored; send it again.');
    });
    await openEditableAssistant();
    await typeInto(messageField(), 'Words the server refused');
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(1));
    await pickPlanner();
    await act(async () => { release(); await held; });
    await settleFor(20);
    expect(messageField().textContent).toBe('');
    expect(screen.queryByRole('alert')).toBeNull();
    await pickAssistant();
    await waitFor(() => expect(messageField().textContent).toBe('Words the server refused'));
    expect(screen.getByRole('alert').textContent).toBe('Not sent. Your message was not stored; send it again.');
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    /* The next press sends the words again as a new message, and nothing of the refused one is left to drop. */
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toHaveLength(2));
    expect(inputBodies(requests)[1]).toEqual({ text: 'Words the server refused' });
  });

  /* #2068 item 25: a replace that failed is drawn as what it is, the turn's replacement, so its words do not read as a
     second copy and Try again plainly replaces the marked turn. */
  it.each([
    ['rejected', () => failure(429, 'rate_limited', 'Wait a moment'), 'Not sent. Wait a moment'],
    ['unknown', () => failure(502, 'bad_gateway', 'Upstream unavailable'), 'Delivery is unconfirmed.'],
  ] as const)('[#2068] marks a %s replace as the replacement of the turn it names', async (_, answer, footer) => {
    twoTurnSetup(answer);
    await openEditableAssistant();
    fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
    await typeInto(messageField(), 'Revised prompt');
    await submit();
    expect((await screen.findByRole('alert')).textContent).toContain(footer);
    expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
    expect(markedMessages()).toEqual(['Original prompt']);
    expect(Array.from(drawerElement().querySelectorAll('[data-nc-turn="you"][data-nc-replacement]'))
      .map((said) => said.textContent)).toEqual(['Revised prompt']);
    expect(within(drawerElement()).getByText('Replaces the marked message above')).toBeTruthy();
  });

  /* #2068 item 8: a stored row stands for one send over its lifetime. Once it retired one, a later send with the same
     words that was never stored must stay drawn, or Dismiss would drop words the server never held. */
  it('[#2068] never lets the row that retired one send hide a later equal send that was never stored', async () => {
    const text = 'the same words twice';
    const state = { queued: false, drained: false, inputs: 0 };
    const { client } = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: state.drained ? 'idle' : 'turn_running',
          model: null, reasoning_effort: null, blocked_reason: null, running_turn: null,
          pending: state.queued && !state.drained ? [{ entry_id: 'entry-a', text, rev: 0, queued_at_ms: 5 }] : [],
          pending_overflow: 0 });
      }
      if (request.path.includes(HISTORY_PATH)) return ok(state.drained ? [harnessMessage(1, 'userMessage', { content: [{ text }] })] : []);
      if (!request.path.endsWith('/planner/input')) return undefined;
      state.inputs += 1;
      if (state.inputs > 1) throw new Error('response dropped');
      state.queued = true;
      return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', entry_id: 'entry-a' });
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('button', { name: 'Stop' });
    /* 1. A is confirmed and queued. */
    await typeInto(messageField(), text);
    await submit();
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="entry-a"]')?.textContent).toContain(text));
    /* 2. B, the same words, is pressed after it; its answers are all lost, and it was never stored. */
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await typeInto(messageField(), text);
    await submit();
    expect((await screen.findByRole('alert')).textContent).toContain('Delivery is unconfirmed');
    /* 3. A drains: its row retires A. */
    state.drained = true;
    await act(async () => { await client.invalidateQueries(); });
    await waitFor(() => expect(document.querySelector('[data-nc-pending-entry="entry-a"]')).toBeNull());
    await settleFor(20);
    /* 4. That row is A's: B stays drawn beside it, with its footer. */
    expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(2);
    expect(screen.getByRole('alert').textContent).toContain('Delivery is unconfirmed');
  });

  it.each(['interrupted', 'failed'] as const)('shows one current paused status over a recorded %s result', async (status) => {
    const terminal = { ...harnessMessage(99, '', {}), item_type: null,
      turn_id: 'previous-turn', method: 'turn/completed', turn_error_text: 'Previous outcome reason.',
      params: JSON.stringify({ id: 'previous-turn', status, error: null }) };
    const reason = 'The stop request timed out before the model confirmed that this turn had stopped.';
    const { requests } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) return ok([terminal]);
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id,
        worker_session_id: 'r', phase: 'wedged', model: null, reasoning_effort: null, blocked_reason: reason, running_turn: null });
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await screen.findByRole('button', { name: 'Paused' });
    const thread = drawerElement().querySelector<HTMLElement>('[data-nc-thread]')!;
    expect(within(thread).getAllByRole('status', { name: 'Current response status' })).toHaveLength(1);
    expect(thread.querySelector('[data-nc-turn-outcome]')).toBeNull();
    expect(screen.queryByText('Previous outcome reason.')).toBeNull();
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(inputBodies(requests)).toHaveLength(0);
  });

  it.each(['interrupted', 'failed'] as const)('continues a settled %s only through an explicit composer send', async (status) => {
    const terminal = { ...harnessMessage(99, '', {}), item_type: null,
      turn_id: 'previous-turn', method: 'turn/completed',
      params: JSON.stringify({ id: 'previous-turn', status, error: null }) };
    const { requests } = setup((request) => {
      if (request.path.includes(HISTORY_PATH)) return ok([terminal]);
      if (request.path.endsWith('/planner/input')) return inputAccepted();
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await screen.findByText('Send a message to continue.', { exact: true });
    expect(inputBodies(requests)).toHaveLength(0);
    await typeInto(messageField(), 'Continue from the partial answer');
    expect(inputBodies(requests)).toHaveLength(0);
    await submit();
    await waitFor(() => expect(inputBodies(requests)).toEqual([{ text: 'Continue from the partial answer' }]));
    expect(screen.queryByText('Send a message to continue.', { exact: true })).toBeNull();
  });

  it('keeps a true receipt as a pending stop until the runtime and transcript confirm completion', async () => {
    let phase = 'turn_running';
    const terminal = { ...harnessMessage(99, '', {}), item_type: null,
      turn_id: 'stopped-turn', method: 'turn/completed',
      params: JSON.stringify({ id: 'stopped-turn', status: 'interrupted', error: null }) };
    let rows: Array<ReturnType<typeof harnessMessage> | typeof terminal> = [];
    let resolve!: (response: ApiTransportResponse) => void;
    const pending = new Promise<ApiTransportResponse>((done) => { resolve = done; });
    const { client, requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id,
        worker_session_id: 'r', phase, model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
      if (request.path.includes(HISTORY_PATH)) return ok(rows);
      if (request.path.endsWith('/planner/interrupt')) return pending;
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    fireEvent.click(await screen.findByRole('button', { name: 'Stop' }));
    await screen.findByRole('button', { name: 'Requesting stop', expanded: false });
    fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    expect(document.querySelector('[data-nc-turn-outcome]')).toBeNull();
    expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(1);
    phase = 'issuing_interrupt';
    await act(async () => { resolve(ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', stopped: true })); await pending; });
    await screen.findByRole('button', { name: 'Stopping', expanded: false });
    expect(document.querySelector('[data-nc-turn-outcome]')).toBeNull();
    phase = 'turn_completed';
    rows = [terminal];
    await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', ASSISTANT_CARD.id] });
      await client.invalidateQueries({ queryKey: ['harness-items', ASSISTANT_CARD.id] }); });
    await screen.findByRole('button', { name: /^Interrupted/ });
    expect(screen.queryByRole('button', { name: 'Stopping' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Stop' })).toBeNull();
  });

  it.each(['load earlier', 'roll latest page'] as const)('keeps an accepted stop pending when history pages %s', async (change) => {
    const current = harnessMessage(100, 'agentMessage', { content: [{ text: 'Still working' }] });
    const historical = { ...harnessMessage(10, '', {}), item_type: null,
      turn_id: 'previous-turn', method: 'turn/completed',
      params: JSON.stringify({ id: 'previous-turn', status: 'completed', error: null }) };
    const { client, requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id,
        worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
      if (request.path.includes(HISTORY_PATH)) return ok(change === 'load earlier' ? [current] : [current, historical]);
      if (request.path.endsWith('/planner/interrupt')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', stopped: true });
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    fireEvent.click(await screen.findByRole('button', { name: 'Stop' }));
    await screen.findByRole('button', { name: 'Stopping', expanded: false });
    const key = cachedHistoryKey(client, ASSISTANT_CARD.id);
    await act(async () => { client.setQueryData(key, { pages: change === 'load earlier' ? [[current], [historical]] : [[current]],
      pageParams: change === 'load earlier' ? [undefined, 100] : [undefined] }); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(screen.getByRole('button', { name: 'Stopping', expanded: false })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(1);
  });

  it.each(['issuing_turn', 'turn_running'])('shows an unconfirmed stop receipt without inventing a terminal result (%s)', async (phase) => {
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id,
        worker_session_id: 'r', phase, model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
      if (request.path.endsWith('/planner/interrupt')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', stopped: false });
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await screen.findByRole('button', { name: 'Stop' });
    await typeInto(messageField(), 'Keep the stop draft');
    fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    const notice = await screen.findByRole('button', { name: 'Stop unconfirmed', expanded: false });
    fireEvent.click(notice);
    expect(screen.getByText('The stop may not have taken effect: the response may still be running or may already have ended.', { exact: true })).toBeTruthy();
    expect(messageField().textContent).toBe('Keep the stop draft');
    expect(document.querySelector('[data-nc-turn-outcome]')).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
    expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(1);
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(0);
  });

  it('shows a refused stop request through the native status row and permits a manual retry', async () => {
    let stops = 0;
    setup((request) => {
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id,
        worker_session_id: 'r', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
      if (request.path.endsWith('/planner/interrupt')) {
        stops += 1;
        return stops === 1 ? failure(409, 'planner_harness_dormant', 'No live planner harness session.')
          : ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', stopped: false });
      }
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    fireEvent.click(await screen.findByRole('button', { name: 'Stop' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Stop failed', expanded: false }));
    expect(screen.getByText('No live planner harness session.', { exact: true })).toBeTruthy();
    expect(screen.queryByRole('alert')).toBeNull();
    expect(document.querySelector('[data-nc-turn-outcome]')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    await screen.findByRole('button', { name: 'Stop unconfirmed', expanded: true });
    expect(stops).toBe(2);
  });

  it('shows the unconfirmed stop reason once and blocks further sends', async () => {
    const reason = 'The stop request timed out before the model confirmed that this turn had stopped.';
    const { requests } = setup((request) => request.path.endsWith('/planner/run')
      ? ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase: 'wedged', model: null,
        reasoning_effort: null, blocked_reason: reason, running_turn: null }) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    const disclosure = await screen.findByRole('button', { name: 'Paused', expanded: false });
    fireEvent.click(disclosure);
    expect(screen.getAllByText(reason, { exact: true })).toHaveLength(1);
    expect(screen.queryByRole('alert')).toBeNull();
    /* The notice's own rule, not the drawer's resize edge. */
    expect(screen.getAllByRole('separator').filter((separator) => separator.getAttribute('aria-label') !== 'Resize conversation')).toHaveLength(1);
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(screen.queryByRole('button', { name: 'Stop' })).toBeNull();
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(0);
    expect(screen.queryByRole('button', { name: 'Start a new conversation' })).toBeNull();
  });

  it('[F6] replaces stale Working with a stuck explanation and preserves the unsent draft', async () => {
    let phase = 'turn_running';
    const { client, requests } = setup((request) => {
      if (request.path === CONVERSATIONS) return ok([assistantRow({ state: 'turn_pending' })]);
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase, model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Assistant/ }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await typeInto(messageField(), 'Draft written before the stall');
    phase = 'wedged';
    await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', ASSISTANT_CARD.id] }); });
    expect(await screen.findByRole('button', { name: 'Paused', expanded: false })).toBeTruthy();
    expect(screen.getByText('This conversation is stuck.', { exact: true })).toBeTruthy();
    expect(drawerWorkingMark()).toBeNull();
    expect(messageField().textContent).toBe('Draft written before the stall');
    expect(messageField().getAttribute('contenteditable')).toBe('false');
    expect(screen.queryByRole('button', { name: 'Stop' })).toBeNull();
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    expect(messageField().textContent).toBe('Draft written before the stall');
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(0);
    expect(screen.queryByRole('button', { name: 'Start a new conversation' })).toBeNull();
  });

  it('[F6] stops promising queued delivery after the harness becomes wedged', async () => {
    let phase = 'turn_running';
    const { client, requests } = setup((request) => request.path.endsWith('/planner/run')
      ? ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase, model: null, reasoning_effort: null, blocked_reason: null, running_turn: null }) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('button', { name: 'Stop' });
    await typeInto(messageField(), 'Queued before the stall');
    await submit();
    await screen.findByText('Queued · sends when this turn ends');
    phase = 'wedged';
    await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', ASSISTANT_CARD.id] }); });
    expect(await screen.findByRole('button', { name: 'Paused', expanded: false })).toBeTruthy();
    expect(screen.getByText('This conversation is stuck.', { exact: true })).toBeTruthy();
    expect(within(drawerElement()).getByText('Queued before the stall', TRANSCRIPT_TEXT)).toBeTruthy();
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
      if (request.path.endsWith('/planner/run')) return ok({ card_id: ASSISTANT_CARD.id, worker_session_id: 'r', phase, model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('Keep the pending message');
    expect(drawerWorkingMark()).not.toBeNull();
    phase = 'wedged';
    await act(async () => { await client.invalidateQueries({ queryKey: ['planner-run', ASSISTANT_CARD.id] }); });
    expect(await screen.findByRole('button', { name: 'Paused', expanded: false })).toBeTruthy();
    expect(screen.getByText('This conversation is stuck.', { exact: true })).toBeTruthy();
    expect(drawerWorkingMark()).toBeNull();
    expect(within(drawerElement()).getByText('Keep the pending message', TRANSCRIPT_TEXT)).toBeTruthy();
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

  /* #2068: a key bound to another first message can never be answered for this one, so the only way
   * forward is a new conversation under a new key; a Try again under the reused key is not offered. */
  it('offers no retry after a key-reused answer, only a new conversation', async () => {
    const { requests } = setup((request) => {
      if (request.path !== CONVERSATIONS || request.method !== 'POST') return undefined;
      return creates(requests, CONVERSATIONS).length === 1
        ? failure(409, 'idempotency_key_reused', 'this key was already used for another first message')
        : created(derivedRow('w1', request));
    });

    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('a key that is spent');
    await screen.findByText('this key was already used for another first message');
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    fireEvent.click(await screen.findByRole('button', { name: 'Send as a new conversation' }));

    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(2));
    const [reused, fresh] = creates(requests, CONVERSATIONS);
    expect(fresh?.headers?.['Idempotency-Key'])
      .not.toBe(reused?.headers?.['Idempotency-Key']);
  });

  /* #2175 (S7): the new conversation's create is refused by its mutation's own guard after the press passed its
     checks. Nothing went out under the fresh key, so the draft is "not started", never unconfirmed, and its Try again
     sends under that fresh key. */
  it('[#2175] says a new conversation was not started when its create is refused before it is sent', async () => {
    const { client, requests } = setup((request) => {
      if (request.path !== CONVERSATIONS || request.method !== 'POST') return undefined;
      return creates(requests, CONVERSATIONS).length === 1
        ? failure(409, 'idempotency_key_reused', 'this key was already used for another first message')
        : created(derivedRow('w1', request));
    });
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await openDraft();
    await write('a key that is spent');
    const sendAsNew = await screen.findByRole('button', { name: 'Send as a new conversation' });
    await waitFor(() => expect(sendAsNew.hasAttribute('disabled')).toBe(false));
    /* Offline from the moment the create's mutation is built: its own guard refuses it, after every earlier check passed. */
    const unsubscribe = client.getMutationCache().subscribe((event) => { if (event.type === 'added') onlineManager.setOnline(false); });
    try {
      fireEvent.click(sendAsNew);
      await waitFor(() => expect(screen.getByRole('alert').textContent).toContain(CONVERSATION_CREATE_TEXT.refused));
    } finally {
      unsubscribe();
      onlineManager.setOnline(true);
    }
    expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
    expect(screen.queryByRole('button', { name: 'Send as a new conversation' })).toBeNull();
    fireEvent.click(await screen.findByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(2));
    const [reused, fresh] = creates(requests, CONVERSATIONS);
    expect(fresh?.headers?.['Idempotency-Key']).not.toBe(reused?.headers?.['Idempotency-Key']);
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

    /* Conversation B, on the same panel instance, while A's send is still out in A's outbox. */
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
    expect(within(drawer).getByText('Loading conversation…', TRANSCRIPT_TEXT)).toBeTruthy();
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

  /* #2131 S7: a create's failure reads through its table. A lost answer is the create's fixed unconfirmed state, a press
     that sent nothing its refusal; neither shows transport text or speaks of the connection, which is the global
     indicator's. The draft keeps its words and its Try again. */
  describe('[#2131 S7] a conversation create that fails', () => {
    const RAW_OR_CONNECTIVITY = /Transport request failed|timed out|schema|offline|reconnect|connection|Nothing was sent|连接/i;
    /** The draft's footer says exactly `text`, beside its remedy, and nothing raw. */
    const says = async (text: string) => {
      await waitFor(() => expect(within(screen.getByRole('alert')).getByText(text, { exact: true })).toBeTruthy());
      expect(screen.getByRole('alert').textContent).not.toMatch(RAW_OR_CONNECTIVITY);
    };
    const lostCreate = (request: ApiRequest) => {
      if (request.method === 'POST' && request.path === CONVERSATIONS) throw new Error('socket hang up');
      return undefined;
    };

    it('a lost answer: says the create is unconfirmed and keeps the words and Try again', async () => {
      setup(lostCreate);
      await screen.findByRole('button', { name: 'Conversation Planner chat' });
      await openDraft();
      await write('words whose answer is lost');
      await screen.findByRole('button', { name: 'Try again' });
      await says(CONVERSATION_CREATE_TEXT.unknown);
      expect(screen.getByText('words whose answer is lost')).toBeTruthy();
    });

    /* #2175: the look for the first attempt's row failing is the same unknown outcome, in the same words. */
    it('changed words after a lost answer whose look-back fails: unconfirmed, with nothing sent under a new key', async () => {
      const { requests } = setup((request) => {
        if (request.path !== CONVERSATIONS) return undefined;
        if (request.method === 'POST') throw new Error('socket hang up');
        return creates(requests, CONVERSATIONS).length > 0 ? failure(503, 'unavailable', 'List unavailable') : undefined;
      });
      await screen.findByRole('button', { name: 'Conversation Planner chat' });
      await openDraft();
      await write('words whose answer is lost');
      await screen.findByRole('button', { name: 'Try again' });
      await write('changed words');
      await says(CONVERSATION_CREATE_TEXT.unknown);
      expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
      expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
    });

    /* #2175 (review of #2226): the same unknown outcome after a "Send as a new conversation" press names that press,
       the only remedy the footer offers there. */
    it('a new conversation whose look-back fails: unconfirmed, offering the new conversation again', async () => {
      let listFails = false;
      const { requests } = setup((request) => {
        if (request.path !== CONVERSATIONS) return undefined;
        if (request.method === 'POST') return failure(409, 'idempotency_key_reused', 'this key was already used for another first message');
        return listFails ? failure(503, 'unavailable', 'List unavailable') : undefined;
      });
      await screen.findByRole('button', { name: 'Conversation Planner chat' });
      await openDraft();
      await write('a key that is spent');
      const sendAsNew = await screen.findByRole('button', { name: 'Send as a new conversation' });
      await waitFor(() => expect(sendAsNew.hasAttribute('disabled')).toBe(false));
      listFails = true;
      fireEvent.click(sendAsNew);
      await says(conversationCreateUnknownText('new-conversation'));
      expect(screen.getByRole('button', { name: 'Send as a new conversation' })).toBeTruthy();
      expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
      expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
    });

    it('offline at the press: says the conversation was not started, and sends nothing', async () => {
      const { requests } = setup();
      await screen.findByRole('button', { name: 'Conversation Planner chat' });
      await openDraft();
      onlineManager.setOnline(false);
      try {
        await write('words typed offline');
        await says(CONVERSATION_CREATE_TEXT.refused);
        expect(creates(requests, CONVERSATIONS)).toHaveLength(0);
        expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
      } finally { onlineManager.setOnline(true); }
    });

    it('offline after a lost answer: stays unconfirmed, since the first attempt may have started it', async () => {
      const { requests } = setup(lostCreate);
      await screen.findByRole('button', { name: 'Conversation Planner chat' });
      await openDraft();
      await write('words sent once');
      const retry = await screen.findByRole('button', { name: 'Try again' });
      onlineManager.setOnline(false);
      try {
        fireEvent.click(retry);
        await says(CONVERSATION_CREATE_TEXT.unknown);
        expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
      } finally { onlineManager.setOnline(true); }
    });

    it('not admitted at the press (bundled recovery not connected): refused, with nothing sent', async () => {
      const access = new RecoveryAccess(); access.change('connected');
      const { requests } = setup(undefined, undefined, access);
      await screen.findByRole('button', { name: 'Conversation Planner chat' });
      await openDraft();
      act(() => { access.change('offline'); });
      await write('words not admitted');
      await says(CONVERSATION_CREATE_TEXT.refused);
      expect(creates(requests, CONVERSATIONS)).toHaveLength(0);
    });

    it('the recovery state changes while the create is out: unconfirmed, never "connection changed"', async () => {
      const access = new RecoveryAccess(); access.change('connected');
      let answer!: (response: ApiTransportResponse) => void;
      const { requests } = setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
        ? new Promise<ApiTransportResponse>((resolve) => { answer = resolve; }) : undefined, undefined, access);
      await screen.findByRole('button', { name: 'Conversation Planner chat' });
      await openDraft();
      await write('words in flight');
      await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));
      act(() => { access.change('offline'); });
      await act(async () => { answer(created(derivedRow('w1', creates(requests, CONVERSATIONS)[0]))); await Promise.resolve(); });
      await says(CONVERSATION_CREATE_TEXT.unknown);
      expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
    });
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
      useEffect(() => { latestSend = (text) => { void send(ASSISTANT_CARD.id, text, [], true, null); }; });
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

  /* #2068 item 17: Edit takes a failed send back into the composer, which only a send that was not stored may do; the
     outbox refuses it for an unknown one itself, not only by the footer offering no Edit there. */
  it('[#2068] refuses to take a spent unknown send back for an Edit', async () => {
    const transport: ApiTransportPort = {
      send: (request) => request.path.endsWith('/planner/input') ? Promise.reject(new Error('response dropped'))
        : Promise.resolve(request.path.endsWith('/planner/run') ? runIdle() : ok([])),
    };
    let store!: ReturnType<typeof useConversationStore>;
    let composerText = '';
    function Probe() {
      const current = useConversationStore(transport, unauthorized, SCOPE, { rows: ROWS, rememberOn: 'w1' });
      const composer = useConversationRegistry().composerOf(ASSISTANT_CARD.id);
      useEffect(() => { store = current; composerText = composer.text; });
      return null;
    }
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
    render(<QueryClientProvider client={client}><ConversationProvider><Probe /></ConversationProvider></QueryClientProvider>);
    await waitFor(() => expect(store.historyReady).toBe(true));
    await act(async () => { void store.send(ASSISTANT_CARD.id, 'maybe stored', [], true, null); await Promise.resolve(); });
    await waitFor(() => expect(store.failedSend?.delivery).toBe('unknown'));
    const key = store.failedSend?.key ?? '';
    act(() => { store.discardFailedSend(key); });
    expect(store.failedSend?.key).toBe(key);
    expect(composerText).toBe('');
  });

  /* Recorded per commit (a layout effect), since `act` would flush the render a switch leaks before any assertion. */
  it('[#2041] shows the next conversation no part of the last one’s send or error, not for one commit', async () => {
    let answer!: () => void;
    const answered = new Promise<void>((done) => { answer = done; });
    const transport: ApiTransportPort = {
      async send(request) {
        if (request.path.endsWith('/planner/input')) { await answered; return inputAccepted(); }
        if (request.path.endsWith('/planner/model')) return failure(500, 'internal', 'model store unavailable');
        if (request.path.endsWith('/planner/run')) return ok({ ...runIdle().body as object, card_id: pathCardId(request.path) });
        return ok([]);
      },
    };
    const PLANNER_SCOPE = { ...SCOPE, cardId: PLANNER_CARD.id };
    type Commit = Readonly<{ cardId: string; sending: boolean; blocked: boolean; pending: number; error: string | null }>;
    const commits: Commit[] = [];
    let latest: ReturnType<typeof useConversationStore> | null = null;
    function StoreProbe({ scope }: { scope: typeof SCOPE }) {
      const store = useConversationStore(transport, unauthorized, scope, { rows: ROWS, rememberOn: 'w1' });
      latest = store;
      useLayoutEffect(() => {
        commits.push({ cardId: scope.cardId, sending: store.sending, blocked: store.sendBlocked, pending: store.pending.size, error: store.actionError });
      });
      return null;
    }
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
    const view = (scope: typeof SCOPE) => (
      <QueryClientProvider client={client}><ConversationProvider><StoreProbe scope={scope} /></ConversationProvider></QueryClientProvider>
    );
    const { rerender } = render(view(SCOPE));
    await act(async () => { void latest?.send(ASSISTANT_CARD.id, 'still out', [], true, null); await Promise.resolve(); });
    await act(async () => { latest?.setModel({ model: 'gpt-x', reasoning_effort: null }); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(commits.at(-1)).toMatchObject({ cardId: ASSISTANT_CARD.id, sending: true, error: 'The model change is unconfirmed.' });
    const before = commits.length;
    await act(async () => { rerender(view(PLANNER_SCOPE)); await Promise.resolve(); });
    const shown = commits.slice(before);
    expect(shown.length).toBeGreaterThan(0);
    expect(shown.every((commit) => commit.cardId === PLANNER_CARD.id)).toBe(true);
    expect(shown.filter((commit) => commit.sending || commit.blocked || commit.pending > 0 || commit.error !== null)).toEqual([]);
    await act(async () => { answer(); await answered; });
  });

  it('[#2068] drops a model refusal that settles while another conversation is shown, not showing it for one commit', async () => {
    let refuse!: () => void;
    const refused = new Promise<void>((done) => { refuse = done; });
    const transport: ApiTransportPort = {
      async send(request) {
        if (request.path.endsWith('/planner/model')) { await refused; return failure(400, 'bad_request', 'Claude is not ready'); }
        if (request.path.endsWith('/planner/run')) return ok({ ...runIdle().body as object, card_id: pathCardId(request.path) });
        return ok([]);
      },
    };
    const errors: (string | null)[] = [];
    let latest: ReturnType<typeof useConversationStore> | null = null;
    function StoreProbe({ scope }: { scope: typeof SCOPE }) {
      const store = useConversationStore(transport, unauthorized, scope, { rows: ROWS, rememberOn: 'w1' });
      latest = store;
      useLayoutEffect(() => { errors.push(store.actionError); });
      return null;
    }
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
    const view = (scope: typeof SCOPE) => (
      <QueryClientProvider client={client}><ConversationProvider><StoreProbe scope={scope} /></ConversationProvider></QueryClientProvider>
    );
    const { rerender } = render(view(SCOPE));
    await act(async () => { latest?.setModel({ model: 'gpt-x', reasoning_effort: null }); await Promise.resolve(); });
    await act(async () => { rerender(view({ ...SCOPE, cardId: PLANNER_CARD.id })); await Promise.resolve(); });
    await act(async () => { refuse(); await new Promise((resolve) => setTimeout(resolve, 0)); });
    await act(async () => { rerender(view(SCOPE)); await Promise.resolve(); });
    expect(errors.filter((error) => error !== null)).toEqual([]);
  });

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
        latestSend = (text) => { void send(ASSISTANT_CARD.id, text, [], true, null); };
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

it.each(['429', 'transport'])('[F5] keeps a %s failure and its Try again when a stale read reveals an old equal message', async (mode) => {
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
  const sent = mode === '429' ? 1 : SEND_RETRIES + 1;
  expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(sent);
  await screen.findByText('Previously completed response');
  expect(screen.getByRole('alert').textContent).toContain(mode === '429' ? 'Not sent' : 'Delivery is unconfirmed');
  expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
  /* A rejected send was not stored, so it is drawn beside the equal row. A spent unknown one may have been: the row
     is drawn once in its place (#2068 item 7), and the send keeps its Try again, which only replays its key. */
  expect(within(drawerElement()).getAllByText(text, TRANSCRIPT_TEXT)).toHaveLength(mode === '429' ? 2 : 1);
  expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(sent);
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
      models: [{ id: 'preset-fast', model: 'gpt-5', resolved_model: null, display_name: 'GPT-5', description: '', is_default: false,
        supported_reasoning_efforts: [{ reasoning_effort: 'low', description: 'Faster' }, { reasoning_effort: 'high', description: 'Thinks longer' }], default_reasoning_effort: 'high' }],
      default: { model: 'gpt-5', reasoning_effort: 'high', supported_reasoning_efforts: null }, default_source: 'config_read', source: 'live', fetched_at_ms: 1,
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
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_reopen: false, can_close: true,
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
      models: [{ id: 'preset', model: 'chosen-model', resolved_model: null, display_name: 'Chosen model', description: '', is_default: false,
        supported_reasoning_efforts: [], default_reasoning_effort: 'low' }],
      default: { model: 'default-model', reasoning_effort: null, supported_reasoning_efforts: null }, default_source: 'config_read', source: 'live', fetched_at_ms: 1,
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
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_reopen: false, can_close: true,
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
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_reopen: false, can_close: true,
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
    if (request.path === '/api/tracks/w1') return ok({ track: TRACK, can_reopen: false, can_close: true,
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


const sideVersion = () => ok({ conversationSide: true, webCompatVersion: 1, minWebCompatVersion: 1, syncEventVersion: 1, dbInstanceId: 'side-db' });

describe('side conversations', () => {
  it('keeps the parent open, creates an independent child with a text snapshot and reopens it', async () => {
    let child: Row & { sourceCardId: string } | null = null;
    const { requests } = setup((request) => {
      if (request.path === '/api/version') return sideVersion();
      if (request.path === CONVERSATIONS && request.method === 'POST') {
        child = { ...derivedRow('w1', request), sourceCardId: ASSISTANT_CARD.id };
        return created(child);
      }
      if (request.path === CONVERSATIONS) return ok([assistantRow(), ...(child === null ? [] : [child])]);
      if (request.path.startsWith(`/api/cards/${ASSISTANT_CARD.id}/harness/items`)) return ok([
        harnessMessage(1, 'agentMessage', { id: 'reply', type: 'agentMessage', text: 'Parent explanation' }),
      ]);
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByText('Parent explanation');
    await write('/side Why this design?');
    const branch = await screen.findByRole('region', { name: 'Side conversation · Codex' });
    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));
    expect(creates(requests, CONVERSATIONS)[0].body).toMatchObject({ text: 'Why this design?',
      side: { source_card_id: ASSISTANT_CARD.id, context: 'Assistant: Parent explanation' } });
    expect(screen.getByRole('complementary', { name: 'Assistant' })).toBeTruthy();
    await waitFor(() => expect(within(branch).getByRole('combobox', { name: 'Message' }).getAttribute('contenteditable')).toBe('true'));
    const field = within(branch).getByRole('combobox', { name: 'Message' });
    await typeInto(field, 'Follow-up');
    fireEvent.keyDown(field, { key: 'Enter' });
    await waitFor(() => expect(requests.some((request) => request.method === 'POST'
      && request.path === `/api/cards/${child!.id}/planner/input`)).toBe(true));
    expect(requests.filter((request) => request.method === 'POST'
      && request.path === `/api/cards/${ASSISTANT_CARD.id}/planner/input`)).toEqual([]);
    fireEvent.click(within(branch).getByRole('button', { name: 'Close side conversation' }));
    await waitFor(() => expect(screen.queryByRole('region', { name: 'Side conversation · Codex' })).toBeNull());
    await write('/side');
    await screen.findByRole('region', { name: 'Side conversation · Codex' });
    expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
  });

  it('keeps an unsent side draft separate from the parent composer', async () => {
    setup((request) => request.path === '/api/version' ? sideVersion() : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('/side');
    const branch = await screen.findByRole('region', { name: 'Side conversation · Codex' });
    const field = within(branch).getByRole('combobox', { name: 'Message' });
    await typeInto(field, 'Unsent side words');
    fireEvent.click(within(branch).getByRole('button', { name: 'Close side conversation' }));
    await waitFor(() => expect(screen.queryByRole('region', { name: 'Side conversation · Codex' })).toBeNull());
    expect(messageField().textContent).toBe('');
    await write('/side');
    const reopened = await screen.findByRole('region', { name: 'Side conversation · Codex' });
    expect(within(reopened).getByRole('combobox', { name: 'Message' }).textContent).toBe('Unsent side words');
  });
});


it('refuses side creation on an older server without sending the command to the parent', async () => {
  const { requests } = setup((request) => request.path === '/api/version'
    ? ok({ webCompatVersion: 1, minWebCompatVersion: 1, syncEventVersion: 1, dbInstanceId: 'old-db' }) : undefined);
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  await write('/side Keep this question');
  await screen.findByText('Side conversations require a server that reports support. Update or reconnect, then try again.');
  expect(messageField().textContent).toBe('/side Keep this question');
  expect(creates(requests, CONVERSATIONS)).toEqual([]);
  expect(requests.filter((request) => request.method === 'POST' && request.path.endsWith('/planner/input'))).toEqual([]);
});

it('adopts a side conversation whose create reply was lost in its own draft slot', async () => {
  let child: Row & { sourceCardId: string } | null = null;
  const { requests } = setup((request) => {
    if (request.path === '/api/version') return sideVersion();
    if (request.path === CONVERSATIONS && request.method === 'POST') {
      child = { ...derivedRow('w1', request), sourceCardId: ASSISTANT_CARD.id };
      return failure(500, 'internal', 'reply lost');
    }
    if (request.path === CONVERSATIONS) return ok([assistantRow(), ...(child === null ? [] : [child])]);
    return undefined;
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  await write('/side Did it arrive?');
  const branch = await screen.findByRole('region', { name: 'Side conversation · Codex' });
  await waitFor(() => expect(within(branch).getByRole('combobox', { name: 'Message' }).getAttribute('contenteditable')).toBe('true'));
  expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
  expect(screen.getByRole('complementary', { name: 'Assistant' })).toBeTruthy();
  expect(within(branch).queryByText('reply lost')).toBeNull();
});

it('interrupts only the focused pane when parent and side turns both run', async () => {
  const child = { ...assistantRow({ id: 'child-running', title: 'Running side', updatedAt: 40 }), sourceCardId: ASSISTANT_CARD.id };
  const { requests } = setup((request) => {
    if (request.path === '/api/version') return sideVersion();
    if (request.path === CONVERSATIONS) return ok([assistantRow(), child]);
    if (request.path.endsWith('/planner/run')) return ok({ card_id: pathCardId(request.path), worker_session_id: 'r',
      phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null });
    if (request.path.endsWith('/planner/interrupt')) return ok({ card_id: pathCardId(request.path), worker_session_id: 'r', stopped: true });
    return undefined;
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  await write('/side');
  const branch = await screen.findByRole('region', { name: 'Side conversation · Codex' });
  const field = within(branch).getByRole('combobox', { name: 'Message' });
  await waitFor(() => expect(within(branch).getByRole('button', { name: 'Stop' })).toBeTruthy());
  field.focus();
  fireEvent.keyDown(field, { key: 'Escape' });
  await waitFor(() => expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(1));
  expect(requests.find((request) => request.path.endsWith('/planner/interrupt'))?.path).toBe('/api/cards/child-running/planner/interrupt');
  expect(screen.getByRole('complementary', { name: 'Assistant' })).toBeTruthy();
});


it('does not offer an ordinary /new draft in the side-only slot', async () => {
  setup((request) => request.path === '/api/version' ? sideVersion() : undefined);
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  await write('/side');
  const branch = await screen.findByRole('region', { name: 'Side conversation · Codex' });
  await typeInto(within(branch).getByRole('combobox', { name: 'Message' }), '/');
  expect(screen.queryByRole('option', { name: /^new/ })).toBeNull();
  expect(branch.isConnected).toBe(true);
});

it('consumes automatic side submission once before an exhausted retry key changes', async () => {
  let child: Row & { sourceCardId: string } | null = null;
  const { requests } = setup((request) => {
    if (request.path === '/api/version') return sideVersion();
    if (request.path === CONVERSATIONS && request.method === 'POST') {
      if (creates(requests, CONVERSATIONS).length === 1) return failure(409, 'idempotency_key_exhausted', 'key exhausted');
      child = { ...derivedRow('w1', request), sourceCardId: ASSISTANT_CARD.id };
      return created(child);
    }
    if (request.path === CONVERSATIONS) return ok([assistantRow(), ...(child === null ? [] : [child])]);
    return undefined;
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
  await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
  await write('/side Retry deliberately');
  const retry = await screen.findByRole('button', { name: 'Try again' });
  await act(async () => { await Promise.resolve(); });
  expect(creates(requests, CONVERSATIONS)).toHaveLength(1);
  fireEvent.click(retry);
  await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(2));
  expect(creates(requests, CONVERSATIONS)[1].body).toEqual(creates(requests, CONVERSATIONS)[0].body);
});
