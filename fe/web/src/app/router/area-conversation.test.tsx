// @vitest-environment jsdom
//
// The area conversation panel, driven through the real router and the real
// transport port (#1098 slice 4).
//
// Everything here is about the two things an area conversation does that a track
// conversation does not: it is listed by the server, and its card does not
// exist until the first message is sent. The rules those two facts imply — the
// `+` mints nothing, the key belongs to the draft rather than to the attempt,
// a 409 is never "it already worked", and none of these rows may leak onto
// Today — are each pinned by one test below.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { areaConversationCardId, AREA_CONVERSATION_TEXT_MAX } from '../../../../core/domain/conversation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { queryKeys } from '../providers/queries.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = { id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
/* A second area, used only where walking between two of them is the point. */
const OTHER_AREA = { id: 'c2', name: 'Home', color: '#000', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };
const CHAT_TRACK_ID = 'chat-track-hidden';
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

type Row = {
  id: string; trackId: string; title: string | null; kind: string;
  state: string | null; updatedAt: number;
};

function row(overrides: Partial<Row> = {}): Row {
  return {
    id: 'chat-1', trackId: CHAT_TRACK_ID, title: null, kind: 'shared-chat',
    state: 'idle', updatedAt: 10, ...overrides,
  };
}

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

function created(body: unknown): ApiTransportResponse {
  return { status: 201, statusText: 'Created', body };
}

function failure(status: number, code: string, error: string): ApiTransportResponse {
  return { status, statusText: 'Error', body: { code, error } };
}

const CONVERSATIONS = '/api/areas/c1/conversations';

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
      if (request.path === '/api/areas/c1/tracks') return ok([TRACK]);
      if (request.path === '/api/overlays?entity_kind=track') return ok([]);
      if (request.path === '/api/tracks/w1') return ok({ track: TRACK, cards: [], overlays: [] });
      if (request.path === CONVERSATIONS) return ok([]);
      if (request.path.includes('/harness/items')) return ok([]);
      if (request.path.endsWith('/planner/run')) return ok({ card_id: 'chat-1', runtime_id: 'r', phase: 'idle' });
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

/*
 * The id the server would give the card this POST mints.
 *
 * Fixtures answer with rows, and a row whose id is invented (`'chat-landed'`)
 * is a row no client could recognise as its own — the panel now looks for the
 * id derived from `(area, key)` rather than for "a row that was not there
 * before", so a fixture that does not derive it the same way is testing a
 * server that does not exist. This mirrors `derive_conversation_keys`
 * through the very function under test's own dependency, and
 * `conversation.test.ts` pins that function against the server's golden.
 */
const derivedId = (request: ApiRequest) =>
  areaConversationCardId('c1', request.headers?.['Idempotency-Key'] ?? '');

const posts = (requests: readonly ApiRequest[]) =>
  requests.filter((request) => request.method === 'POST' && request.path === CONVERSATIONS);

const keysOf = (requests: readonly ApiRequest[]) =>
  posts(requests).map((request) => request.headers?.['Idempotency-Key']);

async function openDraft() {
  fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
  await screen.findByRole('complementary', { name: 'Untitled' });
}

/*
 * `combobox`, not `textbox`. The composer on an area route carries the `/`
 * command menu, and `useTriggerMenu` only emits the combobox role — and the
 * `aria-expanded` / `aria-haspopup` that go with it — when a trigger is
 * actually configured. A track route's composer has no command to offer and so
 * stays a plain `textbox`; that difference is the accessibility tree telling
 * the truth about which field can pop a menu, and this lookup follows it.
 */
function messageField(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

/*
 * Put `text` in the composer and send it.
 *
 * This goes through `typeInto` rather than `fireEvent.change`: the composer is
 * Astryx's `contenteditable` div, which has no value setter, so `change` throws
 * outright — and even if it did not, the field's React state is fed by `input`,
 * so a send driven any other way would post an empty draft and quietly prove
 * nothing about the idempotency guards below.
 */
async function write(text: string) {
  const field = messageField();
  await typeInto(field, text);
  await sendWithEnter(field);
}

/*
 * Send the draft the way the only affordance for it works.
 *
 * There is no `<form>`: `ChatComposer` is a div, and `ChatComposerInput`
 * submits on a bare `Enter` keydown (`Enter` with `shiftKey` inserts a break).
 * So `fireEvent.submit` has nothing to fire at, and Enter is not a stand-in for
 * the real path — it *is* the real path.
 */
async function sendWithEnter(field: HTMLElement) {
  await act(async () => {
    fireEvent.keyDown(field, { key: 'Enter' });
    await Promise.resolve();
  });
}

/*
 * Type into the composer's contentEditable the way a browser does.
 *
 * `fireEvent.change` cannot drive this field — it is a `contenteditable` div,
 * not an `<input>`, so there is no value setter to poke — and the `/` trigger
 * is not driven by the text anyway: `useTriggerMenu` reads the *caret*
 * (`window.getSelection()`), walks backwards from it looking for a trigger
 * character, and does that work on the `input` event. So the caret has to be
 * real. This writes the text, collapses a range at its end inside the editable's
 * own text node, and then fires `input` — which is exactly the sequence the
 * hook is written against, and why a test that only set `textContent` would
 * silently never open the menu.
 */
async function typeInto(field: HTMLElement, text: string) {
  field.textContent = text;
  const range = document.createRange();
  range.setStart(field.firstChild!, text.length);
  range.collapse(true);
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
  /* `await act`, not a bare `fireEvent`: even a *synchronous* SearchSource is
     consumed through `Promise.resolve(...).then(...)` inside `useTriggerMenu`,
     so the items land one microtask after the input event and the menu paints
     `Searching…` until then. */
  await act(async () => {
    fireEvent.input(field);
    await Promise.resolve();
  });
}

const commandMenu = () => screen.queryByRole('listbox', { name: 'Commands' });

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/area/c1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  /* `Math.random` is spied on in one test below; without this it stays frozen
     for every test that runs after it in this file. */
  vi.restoreAllMocks();
});

describe('area conversations', () => {
  it('lists what the server sends, naming it Chat and never its hidden track', async () => {
    setup((request) => request.path === CONVERSATIONS && request.method === 'GET'
      ? ok([row({ title: null }), row({ id: 'chat-2', title: 'Named one', updatedAt: 20 })])
      : undefined);
    await screen.findByRole('button', { name: 'Conversation Chat' });
    await screen.findByRole('button', { name: 'Conversation Named one' });
  });

  it('says nothing about turns it was not told, rather than "undefined turns"', async () => {
    setup((request) => request.path === CONVERSATIONS && request.method === 'GET'
      ? ok([row()]) : undefined);
    const button = await screen.findByRole('button', { name: /^Conversation Chat/ });
    expect(button.getAttribute('aria-label')).toBe('Conversation Chat');
    expect(button.getAttribute('aria-label')).not.toContain('undefined');
  });

  it('leaves the dot unlit for a row with no live session', async () => {
    setup((request) => request.path === CONVERSATIONS && request.method === 'GET'
      ? ok([row({ state: null }), row({ id: 'chat-live', state: 'turn_pending', updatedAt: 20 })])
      : undefined);
    const stateless = await screen.findByRole('button', { name: 'Conversation Chat' });
    const live = await screen.findByRole('button', { name: 'Conversation Chat, live' });
    expect(stateless.getAttribute('aria-label')).not.toContain('live');
    expect(live.getAttribute('aria-label')).toContain('live');
  });

  /*
   * The `+` is not a create button here. The card is minted by the first
   * message, so a `+` that posted would mint a conversation for every reader
   * who opened the drawer and changed their mind.
   */
  it('mints nothing when the drawer is opened', async () => {
    const { requests } = setup();
    await openDraft();
    expect(posts(requests)).toHaveLength(0);
  });

  it('sends the first message once, carrying an idempotency key', async () => {
    const { requests } = setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
      ? created(row({ id: 'chat-new' })) : undefined);
    await openDraft();
    await write('first words');
    await waitFor(() => expect(posts(requests)).toHaveLength(1));
    const [post] = posts(requests);
    expect(post?.body).toEqual({ text: 'first words' });
    expect(post?.headers?.['Idempotency-Key']).toMatch(/[0-9a-f-]{36}/);
  });

  /* The POST already delivered the message: the server started the thread and
     sent it in the same call. A follow-up `/planner/input` would say it twice. */
  it('does not deliver the first message a second time after it succeeds', async () => {
    let rows: Row[] = [];
    const { requests } = setup((request) => {
      if (request.path !== CONVERSATIONS) return undefined;
      if (request.method === 'GET') return ok(rows);
      rows = [row({ id: 'chat-new' })];
      return created(rows[0]);
    });
    await openDraft();
    await write('first words');
    await screen.findByRole('complementary', { name: 'Chat' });
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(0);
  });

  /*
   * The whole point of the header. A key minted per press would be a second
   * derived card id on the retry, so one timeout would become two
   * conversations holding the same first message.
   */
  it('retries a failed first message under the same key, minting no second card', async () => {
    let attempts = 0;
    const { requests } = setup((request) => {
      if (request.method !== 'POST' || request.path !== CONVERSATIONS) return undefined;
      attempts += 1;
      return attempts === 1 ? failure(500, 'internal', 'boom') : created(row({ id: 'chat-new' }));
    });
    await openDraft();
    await write('same words');
    fireEvent.click(await screen.findByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(posts(requests)).toHaveLength(2));
    const keys = keysOf(requests);
    expect(keys[0]).toBe(keys[1]);
    expect(keys[0]).toBeDefined();
    expect(posts(requests).map((request) => request.body))
      .toEqual([{ text: 'same words' }, { text: 'same words' }]);
  });

  /*
   * Closing the drawer is not abandoning the attempt. The key is kept, and `+`
   * is the only door back to it — so if `+` minted a fresh key instead of
   * reopening the draft, the retry would be a second derived card whenever the
   * first attempt had actually committed and lost its answer.
   */
  it('reopens the failed draft under its own key when + is pressed again', async () => {
    let attempts = 0;
    const { requests } = setup((request) => {
      if (request.method !== 'POST' || request.path !== CONVERSATIONS) return undefined;
      attempts += 1;
      return attempts === 1 ? failure(500, 'internal', 'boom') : created(row({ id: 'chat-new' }));
    });
    await openDraft();
    await write('words that failed');
    await screen.findByRole('button', { name: 'Try again' });
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
    await screen.findByRole('complementary', { name: 'Untitled' });
    // The words came back with the drawer; nothing had to be retyped.
    expect(screen.getByText('words that failed')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(posts(requests)).toHaveLength(2));
    const keys = keysOf(requests);
    expect(keys[0]).toBeDefined();
    expect(keys[0]).toBe(keys[1]);
  });

  /*
   * The re-read is a fence, and a fence that cannot read the list has to hold.
   * "I could not look" is not "there is nothing there": the first attempt may
   * have committed with the old text, and a new key on top of that is the
   * second conversation.
   */
  it('does not mint a second key when the list cannot be re-read', async () => {
    // The first attempt *did* commit; its answer was lost, and the network is
    // still bad enough that the list cannot be read to find that out.
    let rows: Row[] = [];
    let listDown = false;
    const { requests } = setup((request) => {
      if (request.path !== CONVERSATIONS) return undefined;
      if (request.method === 'GET') return listDown ? failure(500, 'internal', 'list is down') : ok(rows);
      listDown = true;
      rows = [row({ id: derivedId(request), title: 'Landed anyway' })];
      return failure(500, 'internal', 'boom');
    });
    await openDraft();
    await write('first words');
    await waitFor(() => expect(posts(requests)).toHaveLength(1));
    await screen.findByRole('button', { name: 'Try again' });
    await write('edited words');
    /* The count is asserted first and inside the wait: minting a second key is
       a second POST, and that is the failure — the sentence below is only how
       the reader is told about it. */
    await waitFor(() => {
      expect(posts(requests)).toHaveLength(1);
      expect(screen.getByText(/Could not check whether the last attempt went through/)).toBeTruthy();
    });
    listDown = false;
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await screen.findByRole('complementary', { name: 'Landed anyway' });
    expect(posts(requests)).toHaveLength(1);
  });

  it('keeps the words after a failure instead of losing them with the field', async () => {
    setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
      ? failure(503, 'codex_app_server', 'Agent service is not running') : undefined);
    await openDraft();
    await write('words worth keeping');
    expect((await screen.findByRole('alert')).textContent).toContain('Agent service is not running');
    expect(screen.getByText('words worth keeping')).toBeTruthy();
  });

  it('adopts the conversation a 500 turns out to have created', async () => {
    let rows: Row[] = [];
    setup((request) => {
      if (request.path !== CONVERSATIONS) return undefined;
      if (request.method === 'GET') return ok(rows);
      rows = [row({ id: derivedId(request), title: null })];
      return failure(500, 'internal', 'the send failed after the card was made');
    });
    await openDraft();
    await write('landed anyway');
    await screen.findByRole('complementary', { name: 'Chat' });
    expect(screen.queryByRole('complementary', { name: 'Untitled' })).toBeNull();
  });

  /* The list was simply behind: the key's card was minted by an earlier attempt
     whose answer never arrived. Re-reading turns it up, and that row is the
     conversation this draft was trying to become. */
  it('opens the existing row when the derived card already exists', async () => {
    let rows: Row[] = [];
    setup((request) => {
      if (request.path !== CONVERSATIONS) return undefined;
      if (request.method === 'GET') return ok(rows);
      rows = [row({ id: derivedId(request), title: 'Already here' })];
      return failure(409, 'conflict', 'card already exists');
    });
    await openDraft();
    await write('again');
    await screen.findByRole('complementary', { name: 'Already here' });
  });

  it('prints the unclaimed-folder sentence verbatim and keeps the draft', async () => {
    const sentence = 'area c1 has no claimed folder; claim one before starting a conversation';
    const { requests } = setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
      ? failure(409, 'conflict', sentence) : undefined);
    await openDraft();
    await write('blocked words');
    /* `getByText` with a string is an *exact* match on one element, which is
       what "verbatim" means here. It is scoped inside the alert rather than
       compared against the alert's whole `textContent`, because the strip now
       carries the remedy button in the same region — the sentence still has to
       arrive unedited, but it is no longer the only thing in there. */
    const alert = await screen.findByRole('alert');
    expect(within(alert).getByText(sentence)).toBeTruthy();
    expect(within(alert).getByRole('button', { name: 'Try again' })).toBeTruthy();
    expect(keysOf(requests)).toHaveLength(1);
  });

  it('mints a fresh key once the old one is exhausted, without resending by itself', async () => {
    const { requests } = setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
      ? failure(409, 'idempotency_key_exhausted', 'this key is used up') : undefined);
    await openDraft();
    await write('worn out');
    expect((await screen.findByRole('alert')).textContent).toContain('this key is used up');
    await waitFor(() => expect(posts(requests)).toHaveLength(1));
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(posts(requests)).toHaveLength(2));
    const keys = keysOf(requests);
    expect(keys[0]).not.toBe(keys[1]);
  });

  it('offers a new conversation when the key was spent on different words', async () => {
    let attempts = 0;
    const { requests } = setup((request) => {
      if (request.method !== 'POST' || request.path !== CONVERSATIONS) return undefined;
      attempts += 1;
      return attempts === 1
        ? failure(409, 'conflict', 'operation idempotency key k1 already used with different payload')
        : created(row({ id: 'chat-new' }));
    });
    await openDraft();
    await write('different words');
    await screen.findByRole('alert');
    fireEvent.click(screen.getByRole('button', { name: 'Send as a new conversation' }));
    await waitFor(() => expect(posts(requests)).toHaveLength(2));
    const keys = keysOf(requests);
    expect(keys[0]).not.toBe(keys[1]);
  });

  it('leaves the area for Today when the area itself is gone', async () => {
    setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
      ? failure(404, 'not_found', 'area not found') : undefined);
    await openDraft();
    await write('nowhere to put this');
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/`));
  });

  /*
   * Opening a row must not navigate. Every one of these rows lives on the
   * area's hidden chat track, so `go({name:'track'})` would walk the reader into
   * a track that is deliberately not on any list.
   */
  it('opens a conversation in place, without navigating to its hidden track', async () => {
    const { requests } = setup((request) => request.path === CONVERSATIONS && request.method === 'GET'
      ? ok([row({ id: 'chat-1', title: 'Read me' })]) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Read me' }));
    /*
     * The request, not the URL, is the evidence, and it is checked first.
     *
     * `navigate` is asynchronous: reading `window.location` straight after the
     * click passes even when the app did decide to leave, so that assertion
     * cannot be what pins this. The drawer rendering is only a proxy too — it
     * says something rendered here, not that nothing was fetched over there.
     * Landing on the hidden track fetches its detail; staying never asks for it,
     * so its absence is the one fact that means exactly "did not navigate".
     */
    await waitFor(() => {
      expect(requests.filter(({ path }) => path === `/api/tracks/${CHAT_TRACK_ID}`)).toEqual([]);
      expect(screen.queryByRole('complementary', { name: 'Read me' })).not.toBeNull();
    });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c1`);
  });

  /*
   * The single gate in `useConversationStore`: a `'rows'` route never calls
   * `registry.remember`. Today lists the registry and navigates on open, so a
   * remembered chat row would be a link into the hidden track — and Today has no
   * second filter that would quietly cover for this one.
   */
  it('never leaks an area conversation onto Today', async () => {
    setup((request) => request.path === CONVERSATIONS && request.method === 'GET'
      ? ok([row({ id: 'chat-1', title: 'Read me' })]) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Read me' }));
    await screen.findByRole('complementary', { name: 'Read me' });
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    await screen.findByText('No conversations yet.');
    expect(screen.queryByRole('button', { name: /Conversation Read me/ })).toBeNull();
  });

  /*
   * ── Switching conversations in place builds a new transcript ──────────────
   *
   * The drawer stays mounted across a switch — same route, same `<Drawer>` — so
   * `<ChatThread>` is only rebuilt if it is keyed on the conversation. Without
   * the key it is *reused*, and what is reused with it is the pair of refs the
   * follow-the-newest-turn effect carries: `followsNewest`, which records
   * whether the reader was at the bottom, and `followedTo`, which records the
   * newest turn already followed to. The effect's dependencies
   * (`[turns.length, newestId]`) do fire on the switch — the newest id is a
   * different one — but it then asks `followsNewest.current`, and that answer
   * is about the *previous* conversation. A reader parked in the middle of the
   * first therefore opens the second parked as well: `172` below is that
   * offset, put there by this test rather than measured from anything.
   *
   * Both transcripts are primed and both hold two turns, and that is the whole
   * design of this test: a switch that passed through an empty transcript would
   * remount the component on its own and pass with or without the key.
   */
  it('rebuilds the transcript when another conversation opens in its place', async () => {
    const { client } = setup((request) => request.path === CONVERSATIONS && request.method === 'GET'
      ? ok([row({ id: 'chat-1', title: 'First' }), row({ id: 'chat-2', title: 'Second' })])
      : undefined);
    for (const id of ['chat-1', 'chat-2']) {
      client.setQueryData(queryKeys.harnessItems(id), {
        pages: [harnessTurns(id)], pageParams: [0],
      });
    }
    fireEvent.click(await screen.findByRole('button', { name: /Conversation First/ }));
    await screen.findByRole('complementary', { name: 'First' });

    /* The drawer's own pane, given the geometry jsdom does not compute, so the
       scroll it performs is a value this tier can read. */
    const pane = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]');
    expect(pane).not.toBeNull();
    let offset = 0;
    const writes: number[] = [];
    Object.defineProperty(pane, 'scrollHeight', { configurable: true, value: 800 });
    Object.defineProperty(pane, 'scrollTop', {
      configurable: true,
      get: () => offset,
      set: (value: number) => { offset = value; writes.push(value); },
    });
    /* The reader parks somewhere in the middle of this conversation. */
    offset = 172;

    fireEvent.click(screen.getByRole('button', { name: /Conversation Second/ }));
    await screen.findByRole('complementary', { name: 'Second' });

    /* A conversation opens at its newest turn — always, and not only when it
       happens to hold a different number of them from the one before it. */
    await waitFor(() => { expect(writes).toEqual([800]); });
  });

/** Two turns of transcript for `cardId`, in the shape the harness serves. */
function harnessTurns(cardId: string) {
  return [
    { id: 1, runtime_id: 'r', card_id: cardId, track_id: CHAT_TRACK_ID, thread_id: 't',
      turn_id: null, item_uuid: null, item_type: 'userMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: `ask ${cardId}` } }), created_at_ms: 1 },
    { id: 2, runtime_id: 'r', card_id: cardId, track_id: CHAT_TRACK_ID, thread_id: 't',
      turn_id: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: `answer ${cardId}` } }), created_at_ms: 2 },
  ];
}

  /*
   * The panel is **not** remounted when the reader walks from one area to
   * another: TanStack Router keeps `AreaRoute` mounted and only its params
   * change, so every piece of state in `useConversationPanel` survives the
   * walk. Before the draft named its area, this test posted area 1's key and
   * area 1's words to `/api/areas/c2/conversations` — a conversation minted in
   * the wrong area, from a `+` the reader pressed expecting a blank one.
   *
   * The POST path is what pins it. The drawer's emptiness is a symptom; a
   * request to the wrong area is the damage.
   */
  it('keeps a failed draft to the area it belongs to', async () => {
    const { requests, router } = setup((request) => {
      if (request.path === '/api/areas') return ok([AREA, OTHER_AREA]);
      return request.method === 'POST' && request.path.endsWith('/conversations')
        ? failure(500, 'internal', 'boom')
        : undefined;
    });
    await openDraft();
    await write('words for the first area');
    await screen.findByRole('button', { name: 'Try again' });
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));

    await act(async () => { await router.navigate({ to: '/area/$areaId', params: { areaId: 'c2' } }); });
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2`));
    await openDraft();

    // A blank draft, not the other area's business.
    expect(screen.queryByText('words for the first area')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    await write('words for the second area');
    /* Both areas' POSTs, since the whole question is which area was posted to —
       `posts` only sees area 1's path and would have counted the wrong one. */
    const everyCreate = () => requests
      .filter((request) => request.method === 'POST' && request.path.endsWith('/conversations'));
    await waitFor(() => expect(everyCreate()).toHaveLength(2));
    const [first, second] = everyCreate();
    expect(first?.path).toBe('/api/areas/c1/conversations');
    expect(second?.path).toBe('/api/areas/c2/conversations');
    expect(second?.body).toEqual({ text: 'words for the second area' });
    expect(second?.headers?.['Idempotency-Key']).not.toBe(first?.headers?.['Idempotency-Key']);
  });

  /*
   * A new key is a new attempt, and nothing was posted under it yet.
   *
   * `exhausted` mints one. If the words the *old* key posted survive that mint,
   * the next press compares them against what is typed and reads "the reader
   * edited the text" — a question about a key that no longer exists. The
   * visible cost is the fence that answer triggers: a re-read of the list and,
   * once it comes back empty, a *third* key. So the key the reader was given
   * after the 409 is never the key anything is sent under.
   */
  it('forgets what the spent key sent when it mints a new one', async () => {
    const { requests } = setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
      ? failure(409, 'idempotency_key_exhausted', 'this key is used up') : undefined);
    await openDraft();
    await write('worn out');
    await screen.findByRole('button', { name: 'Try again' });
    await waitFor(() => expect(posts(requests)).toHaveLength(1));
    const afterFirstPost = requests.length;

    // Edited words, deliberately: this is the press that used to be misread.
    await write('worn out, rewritten');
    await waitFor(() => expect(posts(requests)).toHaveLength(2));
    const between = requests.slice(afterFirstPost, requests.indexOf(posts(requests)[1]));
    expect(between.filter((request) => request.method === 'GET' && request.path === CONVERSATIONS))
      .toEqual([]);
    const keys = keysOf(requests);
    expect(keys[0]).not.toBe(keys[1]);
  });

  /*
   * "A row that was not in the list before" is not "this draft's row". While an
   * attempt is failing, another tab — or another reader — can add a
   * conversation to the same area, and adopting it opens somebody else's chat
   * as if it were the words just typed, silently throwing those words away.
   *
   * The card id is a public pure function of `(area, key)`, so the question can
   * be asked exactly (`areaConversationCardId`, golden-tested against the
   * server's own golden in `conversation.test.ts`).
   */
  it('never adopts a row some other client created while it was failing', async () => {
    let rows: Row[] = [];
    const { requests } = setup((request) => {
      if (request.path !== CONVERSATIONS) return undefined;
      if (request.method === 'GET') return ok(rows);
      // Somebody else's brand-new conversation, minted under their own key.
      rows = [row({ id: 'conv-somebody-elses-brand-new-chat', title: 'Not yours' })];
      return failure(500, 'internal', 'boom');
    });
    await openDraft();
    await write('words that are mine');
    await screen.findByRole('button', { name: 'Try again' });
    expect(screen.queryByRole('complementary', { name: 'Not yours' })).toBeNull();
    await screen.findByRole('complementary', { name: 'Untitled' });
    expect(screen.getByText('words that are mine')).toBeTruthy();
    expect(posts(requests)).toHaveLength(1);
  });

  /*
   * A 503 on this endpoint does not mean the request was never served, and the
   * panel used to act as if it did. `create_area_conversation` mints the card
   * through the operation runtime and only then delivers the first message;
   * every 503 it can raise comes from that second half, by which point the card
   * exists. Refusing to look for it left the reader with a "try again" over a
   * conversation that was already there.
   */
  it('adopts the card a 503 left behind, since the mint happens before the send', async () => {
    let rows: Row[] = [];
    setup((request) => {
      if (request.path !== CONVERSATIONS) return undefined;
      if (request.method === 'GET') return ok(rows);
      rows = [row({ id: derivedId(request), title: 'Minted, then the agent stalled' })];
      return failure(503, 'codex_app_server', 'Agent service is not running');
    });
    await openDraft();
    await write('the words that made a card');
    await screen.findByRole('complementary', { name: 'Minted, then the agent stalled' });
  });

  it('has no + on Today, where a conversation has nowhere to attach', async () => {
    setup();
    fireEvent.click(await screen.findByRole('button', { name: 'neige · calm' }));
    await screen.findByText('No conversations yet.');
    expect(screen.queryByRole('button', { name: 'New conversation' })).toBeNull();
  });

  /*
   * The environment the app actually ships into.
   *
   * `crypto.randomUUID` is `[SecureContext]` in the Web Crypto IDL, exactly
   * like `crypto.subtle`. This app is served over plain http on a LAN — no
   * TLS anywhere in `docker/nginx.conf` or the server's listener, and the
   * reader opens `http://<lan-ip>:<port>/calm/` from another machine, which is
   * neither https nor localhost — so `randomUUID` is simply not there and the
   * call that minted the draft key threw. Not a degraded key: no key, no POST,
   * no conversation, in the one place the app runs.
   *
   * jsdom is a secure context and hands out both, which is why nothing caught
   * this. So the insecure context is built here, by taking them away.
   */
  describe('in an insecure context, where randomUUID does not exist', () => {
    const insecureCrypto = (): void => {
      const real = globalThis.crypto;
      vi.stubGlobal('crypto', {
        // `getRandomValues` is the one member of `Crypto` with no
        // `[SecureContext]`, so it is the one that survives.
        getRandomValues: (array: Uint8Array): Uint8Array => real.getRandomValues(array),
      });
    };

    it('still mints a key and sends the first message', async () => {
      insecureCrypto();
      expect((globalThis.crypto as Partial<Crypto>).randomUUID).toBeUndefined();
      expect((globalThis.crypto as Partial<Crypto>).subtle).toBeUndefined();
      let rows: Row[] = [];
      const { requests } = setup((request) => {
        if (request.path !== CONVERSATIONS) return undefined;
        if (request.method === 'GET') return ok(rows);
        rows = [row({ id: derivedId(request), title: 'Started without randomUUID' })];
        return created(rows[0]);
      });
      await openDraft();
      await write('first words with no randomUUID');
      await waitFor(() => expect(posts(requests)).toHaveLength(1));
      expect(posts(requests)[0]?.headers?.['Idempotency-Key'])
        .toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
      // The conversation actually opened, which is the thing that was broken.
      await screen.findByRole('complementary', { name: 'Started without randomUUID' });
    });

    /* And with no `crypto` at all, since the fallback exists to be taken. Two
       keys, not one: a mint that returned a constant would satisfy "a key was
       sent" and break the one thing a key is for. */
    it('mints distinct keys with no Web Crypto at all', async () => {
      vi.stubGlobal('crypto', undefined);
      const { requests } = setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
        ? failure(409, 'idempotency_key_exhausted', 'this key is used up') : undefined);
      await openDraft();
      await write('worn out');
      await screen.findByRole('button', { name: 'Try again' });
      fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
      await waitFor(() => expect(posts(requests)).toHaveLength(2));
      const keys = keysOf(requests);
      expect(keys[0]).toBeDefined();
      expect(keys[0]).not.toBe(keys[1]);
    });

    /*
     * The previous test leans on a real `Math.random`, which hides the thing
     * the fallback has to survive: nothing in the planner makes `Math.random`
     * return distinct values, and an implementation that returns a constant is
     * conforming. With one, sixteen random bytes are sixteen *fixed* bytes and
     * every retry re-sends the key that was just refused — the card id is
     * `sha256(area, key)`, so the retry hits the same card forever.
     *
     * So the worst case is built here: no `crypto`, and a `Math.random` frozen
     * at one value. The keys must still all differ, which only the monotonic
     * counter in `mintIdempotencyKey` can deliver.
     */
    it('mints distinct keys even when Math.random is a constant', async () => {
      vi.stubGlobal('crypto', undefined);
      vi.spyOn(Math, 'random').mockReturnValue(0.42);
      const { requests } = setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
        ? failure(409, 'idempotency_key_exhausted', 'this key is used up') : undefined);
      await openDraft();
      await write('worn out again');
      for (let attempt = 2; attempt <= 5; attempt += 1) {
        fireEvent.click(await screen.findByRole('button', { name: 'Try again' }));
        await waitFor(() => expect(posts(requests)).toHaveLength(attempt));
      }
      const keys = keysOf(requests);
      expect(keys).toHaveLength(5);
      expect(keys.every((key) => typeof key === 'string')).toBe(true);
      expect(new Set(keys).size).toBe(keys.length);
    });
  });

  /*
   * A create is asynchronous and the panel is never remounted, so its answer
   * can arrive in a different area than the one it was sent from.
   *
   * The draft is picked at send time (that much was already true), but adopting
   * the answer used to be two unguarded writes — clear the held draft, aim the
   * drawer at a row — so area A's late success deleted the draft the reader had
   * just started in area B and pointed the drawer at a row area B does not
   * have. Both halves now happen only if the draft they were computed from is
   * still held, in one reducer step.
   */
  it('leaves another area\'s draft alone when a late create finally succeeds', async () => {
    let releaseFirst: ((response: ApiTransportResponse) => void) | null = null;
    const firstCreate = new Promise<ApiTransportResponse>((resolve) => { releaseFirst = resolve; });
    const { requests, router } = setup((request) => {
      if (request.path === '/api/areas') return ok([AREA, OTHER_AREA]);
      if (request.method !== 'POST' || !request.path.endsWith('/conversations')) return undefined;
      // Area 1's create hangs until this test releases it.
      return request.path === CONVERSATIONS ? firstCreate : created(row({ id: 'chat-in-c2' }));
    });
    const everyCreate = () => requests
      .filter((request) => request.method === 'POST' && request.path.endsWith('/conversations'));

    await openDraft();
    await write('words for the first area');
    await waitFor(() => expect(everyCreate()).toHaveLength(1));
    const firstKey = everyCreate()[0]?.headers?.['Idempotency-Key'] ?? '';

    // Off to area 2, where the reader starts a fresh draft while area 1's
    // create is still in the air.
    await act(async () => { await router.navigate({ to: '/area/$areaId', params: { areaId: 'c2' } }); });
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2`));
    await openDraft();

    // Area 1's conversation lands now, while the reader is in area 2.
    await act(async () => {
      releaseFirst?.(created(row({ id: areaConversationCardId('c1', firstKey), title: 'Landed in area one' })));
      await firstCreate;
    });

    /*
     * Area 2's draft is still there and area 1's row was not opened over it.
     * This is one assertion, not two: the unguarded `adopt` did both — it set
     * `held` to null (so the draft drawer had nothing to show) *and* pointed
     * the drawer at area 1's row, which area 2's list does not contain.
     */
    expect(screen.queryByRole('complementary', { name: 'Landed in area one' })).toBeNull();
    /* `data-nc-escape-layer`, not just the label: `Drawer` keeps painting its
       last frame while it retracts, so a drawer that is on its way out still
       answers to the name. The attribute is present only while it is open. */
    const drawer = await screen.findByRole('complementary', { name: 'Untitled' });
    expect(drawer.hasAttribute('data-nc-escape-layer')).toBe(true);

    /* And it is a working draft, not a husk: it sends, and it sends to area 2
       under its own key. */
    await write('words for the second area');
    await waitFor(() => expect(everyCreate()).toHaveLength(2));
    const second = everyCreate()[1];
    expect(second?.path).toBe('/api/areas/c2/conversations');
    expect(second?.body).toEqual({ text: 'words for the second area' });
    expect(second?.headers?.['Idempotency-Key']).not.toBe(firstKey);
  });

  /*
   * The length the client refuses has to be the length the server counts.
   *
   * `create_area_conversation` counts `text.chars().count()` — Unicode scalar
   * values. The panel counted `String.length` — UTF-16 code units — so every
   * astral character (emoji, and most of what is not Latin-adjacent) counted
   * double and a legal message was refused at half the real limit, by the
   * client, with no request and no way for the reader to find out otherwise.
   *
   * (The other half of that mismatch — the server counts the *untrimmed* text
   * and the panel counted the trimmed one — is now aligned in `sendDraft` but
   * is not reachable from here: `ChatComposer` hands `onSend` a trimmed string,
   * so no untrimmed text ever reaches the check. The alignment stays because
   * the panel is not entitled to assume its caller trimmed; there is no test
   * for it because there is no way to make it happen.)
   */
  it('sends an astral-character message that fits, instead of refusing it', async () => {
    /* Exactly at the limit in code points, twice the limit in code units:
       `.length` refused this, and the server would have taken it. */
    const text = '😀'.repeat(AREA_CONVERSATION_TEXT_MAX);
    expect(Array.from(text)).toHaveLength(AREA_CONVERSATION_TEXT_MAX);
    expect(text.length).toBe(AREA_CONVERSATION_TEXT_MAX * 2);
    const { requests } = setup((request) => request.method === 'POST' && request.path === CONVERSATIONS
      ? created(row({ id: 'chat-new' })) : undefined);
    await openDraft();
    await write(text);
    await waitFor(() => expect(posts(requests)).toHaveLength(1));
    expect(posts(requests)[0]?.body).toEqual({ text });
  });

  /*
   * ── `/` in the composer ────────────────────────────────────────────────
   *
   * One command, and it is the `+`'s action reached from the one place the `+`
   * cannot be reached from. `app/shell/shell.module.css` hides the whole panel
   * column while a drawer is open — `.main:has([data-nc-drawer]) [data-nc-panel]
   * { visibility: hidden }` — so a reader inside a conversation has no `+`.
   * That is the gap `/new` fills, and it is why these tests all start from an
   * *open row* rather than from the list.
   */
  describe('the / command menu', () => {
    async function openRowWithMenu(reply?: Reply) {
      const harness = setup(async (request) => await reply?.(request)
        ?? (request.path === CONVERSATIONS ? ok([row()]) : undefined));
      fireEvent.click(await screen.findByRole('button', { name: /Conversation Chat/ }));
      await screen.findByRole('complementary', { name: 'Chat' });
      const field = messageField();
      await typeInto(field, '/');
      return { field, ...harness };
    }

    /*
     * Half the premise, and it is the half jsdom can hold.
     *
     * The command exists because the `+` is unreachable while a drawer is up,
     * and *that* is a CSS fact —
     * `.main:has([data-nc-drawer]) [data-nc-panel] { visibility: hidden }`.
     * jsdom loads no CSS, so it cannot be asserted here: an earlier version of
     * this test claimed it with `plus.closest('[data-nc-panel]')`, a DOM
     * ancestor relation that stays true with that rule deleted from the
     * stylesheet outright — measured, the whole file stayed green.
     *
     * So what is checked here is the part that *is* structural and does hold
     * the premise up on its own terms: the drawer is up, and the only `+` in
     * the document is the one on the column the rule targets — there is no
     * second, unhidden door. The visibility half is pinned where it can fail,
     * in `ui/drawer/public.browser.test.tsx` ("hides the panel column, `+` and
     * all, for as long as a drawer is up"), which presses the `+`'s focus into
     * a real engine. Neither half is worth much alone; the pair is the claim.
     */
    it('leaves exactly one new-conversation door, and it is on the hidden column', async () => {
      await openRowWithMenu();
      expect(document.querySelector('[data-nc-drawer]')).not.toBeNull();
      const plus = screen.getAllByRole('button', { name: 'New conversation' });
      expect(plus).toHaveLength(1);
      expect(plus[0].closest('[data-nc-panel]')).not.toBeNull();
      /* And the `+` is *inside* the region the hiding rule scopes to, which is
         the other end of the selector: a `+` that moved out of `.main` would
         keep its `[data-nc-panel]` ancestor and stop being hidden. */
      expect(plus[0].closest('[data-nc-panel]')?.closest('main')).not.toBeNull();
    });

    it('opens one command on / and describes what it does', async () => {
      await openRowWithMenu();
      const menu = commandMenu();
      expect(menu).not.toBeNull();
      const options = within(menu!).getAllByRole('option');
      expect(options).toHaveLength(1);
      /* The left column is `new` — the rest of what the reader has started
         typing, without the `/` that is already in the field and already on
         screen. Asserted as an exact match on the whole visible row rather
         than `toContain`, because `toContain('new')` would also pass on
         `/new`, on `New conversation`, and on anything else with those three
         letters in it — the point of this line is that the row prints the
         command *and nothing more*, so it has to be able to fail on a stray
         character. The item's `label` is a different string and is what Astryx
         filters on, which is why the next test still types `/new` in full and
         still finds the command.

         The description half of the same string carries what the name cannot:
         "opens a new thread" is already said twice by the `+` and by the word
         `new`, so what is left — and what an exact match now pins — is the
         half a reader cannot guess and would be hurt by getting wrong, that
         the thread they are looking at survives. */
      expect(options[0].textContent).toBe('newThis one stays in the list');
    });

    /* Filtering is Astryx's, on the item label — `/new` has to keep matching
       `New conversation` or the command becomes unreachable by the name the
       user actually types. */
    it('still matches the command once /new is typed in full', async () => {
      const { field } = await openRowWithMenu();
      await typeInto(field, '/new');
      expect(within(commandMenu()!).getAllByRole('option')).toHaveLength(1);
    });

    /*
     * Enter runs it, and the two things that must NOT happen are asserted
     * beside the one that must: the `/new` text is a command, so it may never
     * be delivered as a message, and the field it was typed into must be empty
     * afterwards rather than holding a command the reader has already spent.
     */
    it('runs the command on Enter, sends no message, and clears the field', async () => {
      const { field } = await openRowWithMenu();
      await typeInto(field, '/new');
      fireEvent.keyDown(field, { key: 'Enter' });
      await screen.findByRole('complementary', { name: 'Untitled' });
      expect(field.textContent).toBe('');
      expect(commandMenu()).toBeNull();
    });

    /* Same path as the `+`, so the same thing arrives at the transport: a
       draft mints nothing until a message is sent. */
    it('mints no conversation by itself — the drawer opens on an unsent draft', async () => {
      const { field } = await openRowWithMenu();
      await typeInto(field, '/new');
      fireEvent.keyDown(field, { key: 'Enter' });
      await screen.findByText('Nothing said yet. What you write starts the conversation.');
    });

    /* Arrow keys are consumed by the menu rather than reaching the composer's
       own message-history recall, and they land back on the single item. */
    it('keeps the one item highlighted through ArrowDown and ArrowUp', async () => {
      const { field } = await openRowWithMenu();
      const optionId = within(commandMenu()!).getByRole('option').id;
      expect(field.getAttribute('aria-activedescendant')).toBe(optionId);
      fireEvent.keyDown(field, { key: 'ArrowDown' });
      expect(field.getAttribute('aria-activedescendant')).toBe(optionId);
      fireEvent.keyDown(field, { key: 'ArrowUp' });
      expect(field.getAttribute('aria-activedescendant')).toBe(optionId);
      fireEvent.keyDown(field, { key: 'Enter' });
      expect(commandMenu()).toBeNull();
    });

    /*
     * ── Escape belongs to the menu first ─────────────────────────────────
     *
     * The drawer closes on Escape (`ui/drawer`'s document listener, which
     * checks the topmost `[data-nc-escape-layer]`), and the router has a second
     * document listener in the *capture* phase that interrupts a running turn.
     * A menu that opened inside all of that must still be the thing one Escape
     * closes, or `/` becomes a key you cannot back out of without losing the
     * conversation you were reading.
     *
     * Two mechanisms make that hold, and both are load-bearing: Astryx
     * `preventDefault()`s the Escape it consumes and `ui/drawer` skips any
     * `defaultPrevented` Escape; and the router's capture listener bails on an
     * expanded combobox before it can `stopImmediatePropagation`.
     */
    it('closes the menu on Escape and leaves the drawer open', async () => {
      const { field } = await openRowWithMenu();
      expect(commandMenu()).not.toBeNull();
      fireEvent.keyDown(field, { key: 'Escape' });
      expect(commandMenu()).toBeNull();
      /* Still *open*, not merely still mounted: `ui/drawer` keeps the element
         around for one retract animation, so the element being present proves
         nothing on its own. `data-nc-escape-layer` is written only while
         `open` is true, which is the fact under test. */
      expect(document.querySelector('[data-nc-drawer][data-nc-escape-layer]')).not.toBeNull();
      expect(screen.getByRole('complementary', { name: 'Chat' })).toBeTruthy();
    });

    /*
     * The *other* Escape listener, and the one that actually contends.
     *
     * While a turn is running the router installs a `document` keydown handler
     * in the **capture** phase to interrupt it — capture, so it fires before
     * React has reached the composer at all, and `stopImmediatePropagation`, so
     * whatever it takes nothing else sees. That handler is the only thing that
     * can steal Escape from an open menu, and the only thing stopping it is its
     * expanded-combobox bail. Without a running turn this whole path is dead
     * code, which is why this test — and not the one above — is what pins it.
     */
    it('does not interrupt a running turn with the Escape that closes the menu', async () => {
      const { field, requests } = await openRowWithMenu((request) => request.path.endsWith('/planner/run')
        ? ok({ card_id: 'chat-1', runtime_id: 'r', phase: 'turn_running' })
        : undefined);
      await screen.findByRole('button', { name: 'Stop' });
      fireEvent.keyDown(field, { key: 'Escape' });
      expect(commandMenu()).toBeNull();
      expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(0);
      expect(document.querySelector('[data-nc-drawer][data-nc-escape-layer]')).not.toBeNull();
      /* And the next Escape, with the menu gone, does interrupt — otherwise
         this test would also pass on a build that broke the interrupt. */
      fireEvent.keyDown(field, { key: 'Escape' });
      await waitFor(() => expect(
        requests.filter((request) => request.path.endsWith('/planner/interrupt')),
      ).toHaveLength(1));
    });

    /* And the *second* Escape then does what Escape does when no menu is up.
       This is the half that proves the first test is not passing because the
       drawer stopped listening altogether. */
    it('lets the next Escape close the drawer once the menu is gone', async () => {
      const { field } = await openRowWithMenu();
      fireEvent.keyDown(field, { key: 'Escape' });
      fireEvent.keyDown(document.body, { key: 'Escape' });
      /* Same marker, read the other way. The retracting drawer stays in the
         DOM under jsdom (there is no `animationend` to end the phase), so
         "closed" is the escape layer going away, not the element. */
      await waitFor(() => expect(document.querySelector('[data-nc-drawer][data-nc-escape-layer]')).toBeNull());
    });
  });
});
