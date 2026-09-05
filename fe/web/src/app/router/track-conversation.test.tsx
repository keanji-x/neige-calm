// @vitest-environment jsdom
//
// Starting a conversation on a track (#1189 slice 5), driven through the real
// router and the real transport port — plus what the session registry is
// allowed to remember about one, driven through the real store.
//
// The track route used to fork on whether the track had a planner card: one branch
// opened that card and created nothing, the other offered no `+` at all. It is
// one server-backed rows route now — the list may be empty, and the
// `+` is always there. Everything here is about what that change made possible,
// including the row a track's own list does not contain (the planner card's).
//
// The other half of slice 5 — "and finding one again from Today" — is gone.
// #1341 reversed it: Today lists the launchpad track's own conversations, so a
// track that is not the launchpad reaches Today with nothing, and Today asks no
// other route to open anything. Each place that half was tested says which
// assertion stood there and why it was revoked; the new contract is
// `today-conversation.test.tsx`.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useEffect } from 'react';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { trackConversationCardId } from '../../../../core/domain/conversation.ts';
import { ConversationProvider, useConversationRegistry } from '../conversations/public.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter, useConversationStore } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = { id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
/* The track the old fork left with no way to start anything: no planner card. */
const BARE_TRACK = { ...TRACK, id: 'w2', title: 'Bare track', sort: 2 };
const PLANNER_CARD = { id: 'card-planner', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1, payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
/* The card an assistant conversation is: a codex card carrying the marker the
   kernel persists (`plain_chat.rs::card_is_track_assistant`). */
const ASSISTANT_CARD = { ...PLANNER_CARD, id: 'conv-assistant-1', title: null, payload: { harness_profile: 'assistant' }, sort: 2, updated_at: 30 };
/* A worker card, so "the CARDS panel lists what has a surface" is asserted
   against a track that really has one thing to list. */
const WORKER_CARD = { ...PLANNER_CARD, id: 'card-worker', title: 'Worker', payload: {}, sort: 3, updated_at: 4 };
const cardStatusOverlay = (cardId: string, state: 'AwaitingInput' | 'Errored', updatedAt: number) => ({
  id: `status-${cardId}`, plugin_id: 'kernel', entity_kind: 'card', entity_id: cardId,
  kind: 'status', payload: { state }, updated_at: updatedAt,
});
const trackNeedsInputOverlay = {
  id: 'needs-input', plugin_id: 'kernel', entity_kind: 'track', entity_id: 'w1',
  kind: 'any_card_needs_input', payload: { value: true }, updated_at: 3,
};

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

const CONVERSATIONS = '/api/tracks/w1/conversations';
const BARE_CONVERSATIONS = '/api/tracks/w2/conversations';
const HISTORY_PATH = '/harness/items';

type Row = {
  id: string; trackId: string; title: string | null; kind: string;
  state: string | null; updatedAt: number;
};

function assistantRow(overrides: Partial<Row> = {}): Row {
  return {
    id: ASSISTANT_CARD.id, trackId: 'w1', title: null, kind: 'track-assistant',
    state: 'idle', updatedAt: 30, ...overrides,
  };
}

/**
 * One persisted transcript row, in the shape the harness serves.
 *
 * The two item types do not carry their text the same way — an agent message
 * has `text`, a user message has `content` parts (`harnessItemToTurns`) — so the
 * payload is the caller's to spell, and a row spelled the other way silently
 * yields no turn at all.
 */
function harnessMessage(id: number, itemType: string, item: unknown) {
  return {
    id, runtime_id: 'r', card_id: ASSISTANT_CARD.id, track_id: 'w1', thread_id: 't',
    turn_id: null, item_uuid: null, item_type: itemType, method: 'item/completed',
    params: JSON.stringify({ item, completedAtMs: id }), created_at_ms: id,
  };
}

/** The card a `/api/cards/{id}/…` request is about. */
function pathCardId(path: string): string {
  return decodeURIComponent(path.split('/')[3] ?? '');
}

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

/* The two card-endpoint replies the probes below need, spelled once each: the
   shapes are schema-checked by the transport, and an off-schema body is refused
   before any of these tests can observe anything. */
const inputAccepted = () => ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r' });
const runIdle = () => ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r', phase: 'idle' });

function created(body: unknown): ApiTransportResponse {
  return { status: 201, statusText: 'Created', body };
}

function failure(status: number, code: string, error: string): ApiTransportResponse {
  return { status, statusText: 'Error', body: { code, error } };
}

type Reply = (request: ApiRequest) => ApiTransportResponse | undefined
  | Promise<ApiTransportResponse | undefined>;

function setup(reply?: Reply) {
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
      /* Both card endpoints echo **the card in the path**, which is what the
         kernel does (`cards.rs`'s `/planner/input` answers with the card it just
         accepted input for). A fixture answering with a fixed id is a server
         that does not exist: nothing here reads the field today, so it is not
         a false green yet — it is a trap laid for the first case that does,
         which would then pass against a reply about the wrong conversation. */
      if (request.path.endsWith('/planner/run')) return ok({ card_id: pathCardId(request.path), runtime_id: 'r', phase: 'idle' });
      /* Answering an open conversation's send. The shape matters: an
         off-schema body is refused by the transport, the optimistic echo is
         rolled back, and the name derived from that echo — what the test
         below is about — never exists. */
      if (request.path.endsWith('/planner/input')) return ok({ card_id: pathCardId(request.path), runtime_id: 'r' });
      if (request.path === '/api/settings') return ok({});
      return ok([]);
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
  const router = createAppRouter({ transport, unauthorized, client, onSignOut: vi.fn(), cards: bootTestCardRuntime() });
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

/*
 * The row the POST would mint, named the way the panel names it.
 *
 * The panel looks for the id derived from `(trackId, key)` rather than for "a
 * row that was not there before", so a fixture answering with an invented id is
 * testing a server that does not exist. `conversation.test.ts` pins this
 * function against the kernel's own golden.
 */
const derivedRow = (trackId: string, request: ApiRequest): Row => ({
  id: trackConversationCardId(trackId, request.headers?.['Idempotency-Key'] ?? ''),
  trackId, title: null, kind: 'track-assistant', state: null, updatedAt: 99,
});

async function openDraft() {
  fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
  /* The draft drawer's title, since #1191 renamed it off the action's label:
     the `+` is still "New conversation", the drawer it opens is "Untitled". */
  await screen.findByRole('complementary', { name: 'Untitled' });
}

function messageField(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

/* The composer is Astryx's contenteditable div — no value setter, so `change`
   throws — and it sends on a bare Enter. See `area-conversation.test.tsx`. */
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
  await act(async () => {
    fireEvent.keyDown(field, { key: 'Enter' });
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
          overlays: [trackNeedsInputOverlay, cardStatusOverlay(PLANNER_CARD.id, 'AwaitingInput', 4)],
        })
      : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Review Planner notification' }));
    expect(await screen.findByRole('complementary', { name: 'Planner chat' })).toBeTruthy();
    expect(screen.getByRole('combobox', { name: 'Message' })).toBe(document.activeElement);
    expect(screen.getByRole('region', { name: 'Notifications' })
      .getAttribute('data-nc-notification-mode')).toBe('compact');
    expect(screen.getByRole('region', { name: 'Notifications' }).querySelector('strong')).toBeNull();
  });

  it('opens the requesting worker card instead of the Planner conversation', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackNeedsInputOverlay, cardStatusOverlay(WORKER_CARD.id, 'AwaitingInput', 4)],
        })
      : undefined);

    fireEvent.click(await screen.findByRole('button', { name: 'Review Worker notification' }));
    await waitFor(() => expect(window.location.search).toContain('card=card-worker'));
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-worker"]')).toBeTruthy();
  });

  it('opens an Assistant input notification in its conversation instead of treating it as a worker card', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [trackNeedsInputOverlay, cardStatusOverlay(ASSISTANT_CARD.id, 'AwaitingInput', 4)],
        })
      : undefined);

    fireEvent.click(await screen.findByRole('button', { name: 'Review Assistant notification' }));
    expect(await screen.findByRole('complementary', { name: 'Assistant' })).toBeTruthy();
    expect(screen.getByRole('combobox', { name: 'Message' })).toBe(document.activeElement);
    expect(window.location.search).not.toContain('card=');
  });

  it('lists simultaneous Planner and Worker requests with a truthful count', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [
            trackNeedsInputOverlay,
            cardStatusOverlay(PLANNER_CARD.id, 'AwaitingInput', 4),
            cardStatusOverlay(WORKER_CARD.id, 'Errored', 5),
          ],
        })
      : undefined);

    const notice = await screen.findByRole('region', { name: 'Notifications' });
    expect(within(notice).getByText('2 items need attention')).toBeTruthy();
    expect(within(notice).getByText('Planner')).toBeTruthy();
    expect(within(notice).getByText('Worker')).toBeTruthy();
    expect(within(notice).getByText('Stopped with an error and needs attention.')).toBeTruthy();
    fireEvent.click(within(notice).getByRole('button', { name: 'Collapse notifications' }));
    expect(await screen.findByRole('button', { name: 'Open 2 notifications' })).toBeTruthy();
  });

  it('ignores plugin-authored overlays that imitate the kernel status kind', async () => {
    setup((request) => request.path === '/api/tracks/w1'
      ? ok({
          track: TRACK, can_resume: false,
          cards: [PLANNER_CARD, ASSISTANT_CARD, WORKER_CARD],
          overlays: [
            trackNeedsInputOverlay,
            { ...cardStatusOverlay(WORKER_CARD.id, 'AwaitingInput', 5), plugin_id: 'third-party' },
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

  /*
   * ── G4 ─────────────────────────────────────────────────────────────────────
   *
   * A track with no planner card is exactly the track that most needs to start a
   * conversation, and it was the one track that could not: the route resolved to
   * `'elsewhere'`, which offers no `+` on purpose because Today has no track to
   * attach one to. This track has one — itself.
   *
   * The POST is what is asserted, not the drawer: a `+` that opened a draft
   * nothing could be sent from would satisfy any assertion about the button.
   */
  it('[G4] starts a conversation on a track that has no planner card', async () => {
    /* Stateful on purpose: a real server lists the row it just minted, and the
       create invalidates this very list. A fixture that kept answering `[]`
       would delete the adopted row a round trip later and the assertion below
       would be testing the write-through rather than the product. */
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
    /* Nothing is minted by opening the drawer — the card is minted by the first
       message, as on an area. */
    expect(creates(requests, BARE_CONVERSATIONS)).toHaveLength(0);
    await write('what is in this repo?');
    await waitFor(() => expect(creates(requests, BARE_CONVERSATIONS)).toHaveLength(1));
    const [post] = creates(requests, BARE_CONVERSATIONS);
    expect(post?.body).toEqual({ text: 'what is in this repo?' });
    expect(post?.headers?.['Idempotency-Key']).toMatch(/[0-9a-f-]{36}/);
    /* And the answer is adopted: the drawer moves off the draft onto the row
       the derived id names, which is the one this key would have minted. */
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

  /*
   * A failed create is unfinished work, and its key has to outlive the route
   * that rendered it. The server may have committed the first POST before its
   * 500 reached us; retrying under a freshly-minted key would then create a
   * second conversation beside it.
   *
   * Two tracks matter here. Keeping one draft in a root-level slot merely
   * trades the remount bug for a scope-switch bug: starting track two would
   * overwrite track one's failed attempt. Returning to track one must recover
   * its own words and, decisively, send them under its own original key.
   */
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

  /*
   * The request lifetime has to move with the draft too. If only `{ key,
   * sentText }` survives a remount, the new route instance believes creation
   * is idle. Editing then performs an absent list check while the original POST
   * is still in flight, mints another key, and lets both requests create a row.
   */
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

  /*
   * ── G5 was "a track's conversations reach Today", and #1341 revoked it ──────
   *
   * `[G5] lists every conversation of a track on Today after merely visiting it`
   * stood here. It was the #1189 S5 headline: the track route writes the rows it
   * lists into the session registry, Today lists the registry, so visiting a
   * track put its conversations on Today with a `, on <track>` suffix.
   *
   * Owner reversed that (#1341). Today lists the launchpad track's own
   * conversations from the server, which is the same rule this route follows for
   * itself, and a track that is not the launchpad reaches Today with nothing.
   * The inverse — visiting a track leaves Today's list alone — is asserted in
   * `today-conversation.test.tsx`, where the new contract lives.
   *
   * The registry writes themselves are NOT revoked and are still under test: the
   * `registry write-through` block at the bottom of this file drives the real
   * store against the real registry and asserts what enters it, which is where
   * the four tests that used to read the answer off Today now read it.
   */

  /*
   * Two tests stood here — `[G5] keeps the name it derived from the first
   * message after the drawer closes` and `[G5] does not keep a name, or a time,
   * from a message that failed to send`. Both are alive, and both moved to the
   * `registry write-through` block at the bottom of this file.
   *
   * Neither claim changed. What changed is where the answer is read: they read
   * it off Today's list, and Today no longer lists the registry (#1341). The
   * registry is still written by the same two effects and still read — the
   * drawer's transcript fallback and `turnsBefore` in `send` — so the block
   * below drives the real store under the real provider and asks the registry
   * directly, which is the pattern `mints echo ids that a later mount cannot
   * collide with` already used for the same reason.
   */

  /*
   * The premise the test above rests on, from underneath: **at most one echo is
   * ever unanswered.**
   *
   * "Not yet a fact" is tracked by a single id, and that is only sound while a
   * second send cannot start before the first is answered. `sendingRef` does
   * not deliver that on its own. Walking to another conversation resets it —
   * deliberately, the new conversation's composer must work — and the request
   * left behind then settles into a store that has moved on. Clearing the
   * send state unconditionally on the way out re-opens a composer whose *own*
   * message is still in flight, and the store is then holding two unanswered
   * echoes with one slot to name them: the older one is silently reclassified
   * as confirmed and written into the registry as fact — the exact defect the
   * test above pins, walked in through a different door.
   *
   * The second POST is what this asserts. A composer that re-opens is a symptom
   * you can argue about; a third message this store had no right to accept is
   * the state that produces the wrong registry write.
   */
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

    /* Conversation B, on the same panel instance — the walk that resets
       `sendingRef` so this composer works at all. */
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Planner chat' }));
    await screen.findByRole('complementary', { name: 'Planner chat' });
    await waitFor(() => expect(messageField().getAttribute('contenteditable')).toBe('true'));
    await write('the second conversation speaks');
    await waitFor(() => expect(held.has(PLANNER_CARD.id)).toBe(true));
    const sends = () => requests.filter((request) =>
      request.path === `/api/cards/${PLANNER_CARD.id}/planner/input`);
    expect(sends()).toHaveLength(1);

    /* A's request lands now, and it is answering for a conversation nobody is
       looking at. */
    await act(async () => { release(ASSISTANT_CARD.id); await Promise.resolve(); });
    await act(async () => { await Promise.resolve(); });

    /* B's message is still unanswered, so B's composer is still closed: this
       third message does not go out. */
    await write('and a third the store must refuse');
    await act(async () => { await Promise.resolve(); });
    expect(sends()).toHaveLength(1);
    /* Nor did A's failure surface under B's composer — it is not B's failure,
       and B's reader never sent that message. */
    expect(screen.queryByText(/busy|Could not send/)).toBeNull();
  });

  /*
   * `[G5] does not overwrite a refresh that landed while the send was still
   * settling` and `[G5] counts a message really sent twice, when the refresh is
   * a moment behind` stood here, and moved to the `registry write-through`
   * block for the reason given above: `2 turns` was read off Today's row label,
   * and it is now read off the registry entry that label was rendering.
   */

  /*
   * ── G6 was "open a conversation asked for from Today", and #1341 revoked it ─
   *
   * Three tests stood here: `opens an assistant conversation asked for from
   * Today`, `keeps the open request until the list it names arrives`, and
   * `gives up an open request whose list could not be read`. All three drove
   * the same production path — Today lists a row belonging to another track,
   * navigates there, and leaves the card id in the registry for the arriving
   * route to redeem.
   *
   * Today has no such row any more: it lists the launchpad's own conversations
   * and opens them in place, in its own drawer, navigating nowhere
   * (`today-conversation.test.tsx` asserts exactly that). So the producer of a
   * cross-track open request is gone, and with it the only driver these three
   * had. Keeping them would have meant poking the registry by hand to prove a
   * rule about a request no route makes.
   *
   * The consume itself is NOT dead and is not deleted: #1211's planner-open intent
   * still leaves a request — for a card of the very route that arms it — and
   * `new-track-route.test.tsx` drives it end to end. The two *clears* are what
   * lost their producer; they stay as fail-safes and say so at their site in
   * `app/router/public.tsx`. The cross-track index card, on its own issue, is
   * what would bring the producer back.
   */

  /*
   * ── §5.4 ───────────────────────────────────────────────────────────────────
   *
   * An assistant card is read in the drawer and draws nothing, so it is
   * headless — like the planner card and the report card. Registering the entry is
   * only half of that: `codex` is scanned first and would otherwise claim the
   * card and put an empty terminal in this panel.
   */
  it('keeps assistant cards out of the CARDS panel, listing only the worker', async () => {
    setup();
    /* `[data-nc-card-inventory]` is the CARDS module's own list rather than any
       of the page's other lists — the panel is what the reader reads. */
    const list = await waitFor(() => {
      const found = document.querySelector('[data-nc-card-inventory]');
      if (found === null) throw new Error('card inventory has not rendered');
      return found as HTMLElement;
    });
    const labels = within(list).getAllByRole('listitem').map((row) => row.textContent ?? '');
    /* The row reads its title and then its kernel kind. The assistant card is
       absent entirely — not listed under some other name. */
    expect(labels).toEqual(['Workercodex']);
  });
});

/*
 * ── The registry write-through, asked of the registry ────────────────────────
 *
 * What may enter the session registry, what may not, and what survives a drawer
 * being closed on a send that has not settled. Six invariants, all of them
 * about `useConversationStore`'s two remember effects and its `send`.
 *
 * They are driven through the store itself rather than through the router, and
 * that is a deliberate move rather than a shortcut. Four of them used to read
 * their answer off Today's conversation list, which once used the registry as
 * its source. Since #1341 every list is server-backed; confirmed metadata from
 * the registry is projected onto those rows rather than becoming a separate
 * list. The invariants did not go away — the registry is still read by the
 * drawer's transcript fallback and by `turnsBefore` in `send` — so this block
 * asks the store directly, while the route-level name test above pins the
 * projection a reader sees. The store, provider, query client and transport port
 * here are all the production ones; only the route around them is absent.
 *
 * The drawer is modelled by `scope`, which is exactly what the production panel
 * does: `useConversationPanel` computes `scope` from the open row, so closing
 * the drawer *is* handing this store a null scope while it stays mounted.
 */
describe('registry write-through', () => {
  const SCOPE = {
    id: 'w1', title: 'Test track', cardId: ASSISTANT_CARD.id, cardTitle: null,
    updatedAt: 30, kind: 'track-assistant' as const, state: 'idle' as const,
  };
  const ROWS: readonly Conversation[] = [{
    id: ASSISTANT_CARD.id, trackId: 'w1', trackTitle: 'Test track', title: null,
    kind: 'track-assistant', state: 'idle', updatedAt: 30,
  }];

  /**
   * One mounted store over one registry, with the drawer openable and closable.
   *
   * `rows` is the track's server list, so the batch remember runs for real —
   * which matters: three of the tests below are precisely about that effect
   * writing a plain server row over what the drawer had derived.
   */
  function mountStore(transport: ApiTransportPort, rows: readonly Conversation[] = ROWS) {
    let latestSend: (text: string) => void = () => undefined;
    let known: readonly Conversation[] = [];
    let readTurns: (id: string) => readonly TranscriptEntry[] = () => [];

    function StoreProbe({ scope }: { scope: typeof SCOPE | null }) {
      const store = useConversationStore(transport, unauthorized, scope, {
        rows, rememberOn: 'w1',
      });
      const send = store.send;
      useEffect(() => { latestSend = (text) => { send(ASSISTANT_CARD.id, text); }; });
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
      /*
       * Pressing Enter, and then letting the world turn.
       *
       * The macrotask is not padding. `mutations.send` resolves two query
       * invalidations after its POST, and an invalidation only refetches for
       * observers React has committed — so a send flushed with a bare
       * `Promise.resolve()` resolves with neither refresh having gone out, which
       * is not the state any of these tests is about. Under the router the same
       * pumping is what `findBy*` does between renders.
       */
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

  /*
   * The name a conversation earns, and what re-reading the list does to it.
   *
   * An assistant card is minted with no title and nothing backfills one, so the
   * server's row is `title: null` for the whole life of the conversation. The
   * only name it ever has is the one the drawer derives from its first message,
   * and only the drawer can derive it. The batch remember writes every listed
   * row into the same registry entries — carrying `turns` and the transcript
   * across, and, before this test existed, *not* the name: the moment the
   * drawer closed, the entry fell back to the bare kind label.
   */
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
    /* And the batch remember, which now owns this entry, did not put the
       server's `title: null` back over it. */
    expect(store.entry()?.title).toBe('rename this conversation');
  });

  /*
   * And the name it did **not** earn, which is the same carry-over read from the
   * other side.
   *
   * An echo goes into the store the instant Enter is pressed, named and timed
   * from the browser's own clock, and it is not yet a fact — the POST can still
   * fail. Two rules that are each right compose into a wrong one: the effect
   * that remembers the open conversation, and the batch remember that carries an
   * entry's name and time forward rather than letting a `title: null` row undo
   * them. Close the drawer while the POST is in flight and the optimistic values
   * are what get carried; when the POST then fails, `scope` is already null, so
   * the `catch` that drops the echo reaches nothing and the registry — which has
   * no `forget` — keeps them for the life of the tab.
   *
   * **The order is the test.** Rejecting before the drawer closes exercises the
   * `scope !== null` path, where the open conversation is simply re-remembered
   * without the echo and everything corrects itself; that arrangement is green
   * with or without the fix. The gap is only reachable while the drawer is shut
   * and the request is still out.
   */
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

    /* No name — the kind label is all this conversation has ever earned, which
       is what a `title` of null renders as. */
    expect(store.entry()?.title ?? null).toBeNull();
    /* And no time from a clock only this browser read: the row's own
       `updatedAt` stands, so the entry cannot float above rows that really were
       touched. `beforeSend` is the fence — an echo's `atMs` is `Date.now()`. */
    expect(store.entry()?.updatedAt).toBe(30);
    expect(store.entry()?.updatedAt).toBeLessThan(beforeSend);
    /* Nor is the message itself remembered as something that happened. */
    expect(store.turns()).toHaveLength(0);
  });

  /*
   * The other side of the write-through: it may not undo what arrived while it
   * was waiting.
   *
   * The POST may still be in flight while an event or another read changes the
   * entry legitimately: the server's own copy of the message arrives, the
   * agent's reply arrives with it, and the effects write both into the registry
   * before this send's acknowledgement handler runs, with the drawer already
   * shut.
   *
   * A callback merging into the list it captured when Enter was pressed writes
   * the pre-send entry back: the reply is gone, the count is the one this
   * browser could count on its own, and nothing will ever fetch them again for
   * a conversation whose only surface is a track the reader has left. The read
   * and the write have to be the same moment, which is `updateExisting`.
   *
   * The arriving refresh is fed in with `setQueryData` rather than by holding
   * one of the two invalidations open. Same state, one fewer moving part: what
   * the test needs is a history that lands *between* the POST and its
   * settlement, and the query cache is where a landed history lives —
   * `planner-conversation.test.tsx` drives its refreshes the same way.
   *
   * Two turns is the assertion, and it is one number that rejects both ways of
   * getting this wrong: a captured snapshot says `1` (the echo, on top of an
   * entry that knew nothing), and an atomic merge that appends its echo without
   * noticing the server already sent that message back says `3`.
   */
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
    /* The window opens: the history refresh lands — the reader's own message
       comes back from the server, and the agent has answered it — while the
       POST that started all this is still out. */
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

  /*
   * And the same write-through from the other side: a message the reader really
   * did send twice is two messages.
   *
   * The check above asks "did the refresh already bring this very message
   * back?", and it asks it by text — an echo carries no server id, so text is
   * all there is. Asked against the *whole* entry, an older identical message
   * answers for the new one: `ping` is in the transcript from an hour ago, you
   * type `ping` again, the POST succeeds, and the refresh that follows it is a
   * moment behind the write. The write-through then finds a `ping` it did not
   * mint, calls this one already recorded, and neither appends it nor counts it.
   * `reconcileUserEchoes` pairs one-to-one only among the echoes of a single
   * call, and this call passes one, so nothing tells it the old row is already
   * spoken for. Only the rows that arrived *since the send* can answer the
   * question that was asked.
   *
   * Two turns is the assertion and it fails in the one direction that matters:
   * the old-`ping`-answers-for-the-new bug reports `1`.
   */
  it('[G5] counts a message really sent twice, when the refresh is a moment behind', async () => {
    let releaseInput!: () => void;
    const inputSettled = new Promise<void>((resolve) => { releaseInput = resolve; });
    const transport: ApiTransportPort = {
      async send(request) {
        /* The server's copy of the *first* `ping`, and only ever that one: the
           second is accepted and persisted, and no read brings it back before
           the send settles. */
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
    /* The first `ping` is already in the transcript — the row this test is
       about having to not answer for the next one. */
    await waitFor(() => { expect(store.turns()).toHaveLength(1); });
    await store.send('ping');

    await store.closeDrawer();
    await act(async () => { releaseInput(); await Promise.resolve(); });
    await store.settle();
    expect(store.turns()).toHaveLength(2);
    expect(store.entry()?.turns).toBe(2);
  });

  /* Two real store instances under one provider: the first request crosses the
     remount, the provider lease blocks a same-card second send, and the second
     same-text echo remains distinct when the first server row arrives. */
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
        latestSend = (text) => { send(ASSISTANT_CARD.id, text); };
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
    /* The walk away: this store is gone, its counter with it, and its request
       is still out. */
    await act(async () => { rerender(view(null)); await Promise.resolve(); });
    /* And the walk back — a new store, on the same conversation, under the same
       registry. */
    await act(async () => { rerender(view('second-mount')); await Promise.resolve(); });
    await act(async () => { latestSend('ping'); await Promise.resolve(); });
    /* The provider-wide per-card lease keeps a remount from starting another
       send while the first request is unresolved. */
    expect(holds).toHaveLength(1);

    /* The first POST lands while the second store is mounted, and its refresh
       exposes one `ping`. Only after that request settles may the new store
       start its same-text send. */
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
