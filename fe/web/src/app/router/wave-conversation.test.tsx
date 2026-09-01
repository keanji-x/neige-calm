// @vitest-environment jsdom
//
// Starting a conversation on a wave, and finding one again from Today
// (#1189 slice 5), driven through the real router and the real transport port.
//
// The wave route used to fork on whether the wave had a spec card: one branch
// opened that card and created nothing, the other offered no `+` at all. It is
// one `'rows'` route now — the list is the server's, it may be empty, and the
// `+` is always there. Everything here is about what that change made possible
// and about the two places it could silently not work: the row a wave's own
// list does not contain (the spec card's), and the Today → wave open request,
// which is consumed in one place and thrown away in another.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { waveConversationCardId } from '../../../../core/domain/conversation.ts';
import { queryKeys } from '../providers/queries.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const COVE = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = { id: 'w1', cove_id: 'c1', title: 'Test wave', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
/* The wave the old fork left with no way to start anything: no spec card. */
const BARE_WAVE = { ...WAVE, id: 'w2', title: 'Bare wave', sort: 2 };
const SPEC_CARD = { id: 'card-spec', wave_id: 'w1', kind: 'codex', title: 'Spec chat', sort: 1, payload: { spec_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
/* The card an assistant conversation is: a codex card carrying the marker the
   kernel persists (`plain_chat.rs::card_is_wave_assistant`). */
const ASSISTANT_CARD = { ...SPEC_CARD, id: 'conv-assistant-1', title: null, payload: { harness_profile: 'assistant' }, sort: 2, updated_at: 30 };
/* A worker card, so "the CARDS panel lists what has a surface" is asserted
   against a wave that really has one thing to list. */
const WORKER_CARD = { ...SPEC_CARD, id: 'card-worker', title: 'Worker', payload: {}, sort: 3, updated_at: 4 };

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

const CONVERSATIONS = '/api/waves/w1/conversations';
const BARE_CONVERSATIONS = '/api/waves/w2/conversations';

type Row = {
  id: string; waveId: string; title: string | null; kind: string;
  state: string | null; updatedAt: number;
};

function assistantRow(overrides: Partial<Row> = {}): Row {
  return {
    id: ASSISTANT_CARD.id, waveId: 'w1', title: null, kind: 'wave-assistant',
    state: 'idle', updatedAt: 30, ...overrides,
  };
}

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

function created(body: unknown): ApiTransportResponse {
  return { status: 201, statusText: 'Created', body };
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
      if (request.path === '/api/coves') return ok([COVE]);
      if (request.path === '/api/coves/c1/waves') return ok([WAVE, BARE_WAVE]);
      if (request.path === '/api/coves/c1/conversations') return ok([]);
      if (request.path === '/api/overlays?entity_kind=wave') return ok([]);
      if (request.path === '/api/waves/w1') {
        return ok({ wave: WAVE, cards: [SPEC_CARD, ASSISTANT_CARD, WORKER_CARD], overlays: [] });
      }
      if (request.path === '/api/waves/w2') return ok({ wave: BARE_WAVE, cards: [], overlays: [] });
      if (request.path === CONVERSATIONS) return ok([assistantRow()]);
      if (request.path === BARE_CONVERSATIONS) return ok([]);
      if (request.path.includes('/harness/items')) return ok([]);
      if (request.path.endsWith('/spec/run')) return ok({ card_id: SPEC_CARD.id, runtime_id: 'r', phase: 'idle' });
      /* Answering an open conversation's send. The shape matters: an
         off-schema body is refused by the transport, the optimistic echo is
         rolled back, and the name derived from that echo — what the test
         below is about — never exists. */
      if (request.path.endsWith('/spec/input')) return ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r' });
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

const creates = (requests: readonly ApiRequest[], path: string) =>
  requests.filter((request) => request.method === 'POST' && request.path === path);

/*
 * The row the POST would mint, named the way the panel names it.
 *
 * The panel looks for the id derived from `(waveId, key)` rather than for "a
 * row that was not there before", so a fixture answering with an invented id is
 * testing a server that does not exist. `conversation.test.ts` pins this
 * function against the kernel's own golden.
 */
const derivedRow = (waveId: string, request: ApiRequest): Row => ({
  id: waveConversationCardId(waveId, request.headers?.['Idempotency-Key'] ?? ''),
  waveId, title: null, kind: 'wave-assistant', state: null, updatedAt: 99,
});

async function openDraft() {
  fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
  await screen.findByRole('complementary', { name: 'New conversation' });
}

function messageField(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

/* The composer is Astryx's contenteditable div — no value setter, so `change`
   throws — and it sends on a bare Enter. See `cove-conversation.test.tsx`. */
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
  window.history.pushState({}, '', `${APP_BASEPATH}/wave/w1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('wave conversations', () => {
  it('lists the wave\'s assistant conversations beside the spec one', async () => {
    setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    await screen.findByRole('button', { name: 'Conversation Assistant' });
  });

  /*
   * ── G4 ─────────────────────────────────────────────────────────────────────
   *
   * A wave with no spec card is exactly the wave that most needs to start a
   * conversation, and it was the one wave that could not: the route resolved to
   * `'elsewhere'`, which offers no `+` on purpose because Today has no wave to
   * attach one to. This wave has one — itself.
   *
   * The POST is what is asserted, not the drawer: a `+` that opened a draft
   * nothing could be sent from would satisfy any assertion about the button.
   */
  it('[G4] starts a conversation on a wave that has no spec card', async () => {
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
    await act(async () => { await router.navigate({ to: '/wave/w2' }); });
    await screen.findByText('No conversations yet.');
    await openDraft();
    /* Nothing is minted by opening the drawer — the card is minted by the first
       message, as on a cove. */
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

  it('[G4] sends the first message once, to the wave in the URL and no other', async () => {
    const { requests } = setup((request) =>
      request.method === 'POST' && request.path === CONVERSATIONS
        ? created(derivedRow('w1', request))
        : undefined);
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    await openDraft();
    await write('first words');
    await waitFor(() => expect(creates(requests, CONVERSATIONS)).toHaveLength(1));
    expect(requests.filter((request) => request.method === 'POST'
      && request.path.endsWith('/conversations') && request.path !== CONVERSATIONS)).toEqual([]);
    /* The message travelled with the POST, so nothing re-sends it afterwards. */
    expect(requests.filter((request) => request.path.endsWith('/spec/input'))).toEqual([]);
  });

  /*
   * ── G5 ─────────────────────────────────────────────────────────────────────
   *
   * Visited, never opened — and that is the whole test. Opening a row remembers
   * it through a path that predates this slice; what is under test is the wave
   * route writing the rows it *lists* into the registry, which is the only way
   * an assistant conversation can reach Today at all. It exists nowhere but in
   * that list: no other surface fetches it, and the drawer would have to be
   * opened on this very wave for the older path to see it.
   *
   * Both rows are asserted. The spec-derived row alone would stay green under a
   * remember that only wrote the injected row, which is a strictly smaller fix
   * that leaves the feature's own conversations invisible.
   */
  it('[G5] lists every conversation of a wave on Today after merely visiting it', async () => {
    const { router } = setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    await screen.findByRole('button', { name: 'Conversation Assistant' });
    /* Nothing was opened: `[data-nc-drawer]` is the drawer's own marker, and
       `role="complementary"` alone would match the rail. */
    expect(document.querySelector('[data-nc-drawer]')).toBeNull();

    await act(async () => { await router.navigate({ to: '/' }); });
    const spec = await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave' });
    const assistant = await screen.findByRole('button', { name: 'Conversation Assistant, on Test wave' });
    /*
     * And they are distinguishable **on screen**, not only to a screen reader.
     *
     * Asserting the two `aria-label`s alone is what let the row render the
     * *wave's* title in place of the conversation's: this list's whole point is
     * now that one wave contributes several rows to Today, and while the label
     * carried the difference the visible text of all of them was `Test wave`.
     * A reader with a mouse had N identical rows and no way to choose. So the
     * text is pinned here, both that each row says its own name and that the
     * two rows differ.
     */
    expect(spec.textContent).toBe('Spec chatTest wave');
    expect(assistant.textContent).toBe('AssistantTest wave');
    expect(assistant.textContent).not.toBe(spec.textContent);
  });

  /*
   * The name a conversation earns, and what re-reading the list does to it.
   *
   * An assistant card is minted with no title and nothing backfills one, so the
   * server's row is `title: null` for the whole life of the conversation. The
   * only name it ever has is the one the drawer derives from its first message,
   * and only the drawer can derive it. The batch remember below the drawer
   * writes every listed row into the same registry entries — carrying `turns`
   * and the transcript across, and, before this test existed, *not* the name:
   * the moment the drawer closed, Today fell back to the bare kind label.
   */
  it('[G5] keeps the name it derived from the first message after the drawer closes', async () => {
    const { router } = setup();
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('complementary', { name: 'Assistant' });
    await write('rename this conversation');
    /* The drawer names itself from the transcript — the same derivation the
       registry is about to be given. */
    await screen.findByRole('complementary', { name: 'rename this conversation' });
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));

    await act(async () => { await router.navigate({ to: '/' }); });
    const row = await screen.findByRole('button', {
      /* The turn count is the *other* thing this batch remember carries over,
         and it is on the label for the same reason the name is: this tab read
         it and the list does not send it. */
      name: 'Conversation rename this conversation, on Test wave, 1 turns',
    });
    expect(row.textContent).toBe('rename this conversationTest wave');
    /* Not the kind label it would have fallen back to. */
    expect(screen.queryByRole('button', { name: /^Conversation Assistant,/ })).toBeNull();
  });

  /*
   * The other half of the same decision, and the reason it is a wave id rather
   * than a flag: a cove's rows stay out of the registry. They live on the
   * cove's hidden chat wave, and Today navigates to `conversation.waveId` when
   * a row is opened.
   */
  it('[G5] still keeps a cove\'s own conversations off Today', async () => {
    const { router } = setup((request) => request.path === '/api/coves/c1/conversations'
      ? ok([{ id: 'chat-1', waveId: 'chat-wave-hidden', title: 'Cove chat', kind: 'shared-chat', state: 'idle', updatedAt: 40 }])
      : undefined);
    await act(async () => { await router.navigate({ to: '/cove/c1' }); });
    await screen.findByRole('button', { name: 'Conversation Cove chat' });
    await act(async () => { await router.navigate({ to: '/' }); });
    await screen.findByText('No conversations yet.');
    expect(screen.queryByRole('button', { name: /Conversation Cove chat/ })).toBeNull();
  });

  /*
   * ── G6 ─────────────────────────────────────────────────────────────────────
   *
   * Today can only navigate; the wave has to finish the job. Two things had to
   * change for an **assistant** row, and each of them alone leaves this red:
   *
   *  * `WaveRoute` cleared any open request whose card was not a spec harness,
   *    which threw this one away before the list had even loaded;
   *  * the panel's consume ran off `scope`, which on a `'rows'` route is null
   *    until a row is already open — the very thing being asked for.
   */
  it('[G6] opens an assistant conversation asked for from Today', async () => {
    const { router } = setup();
    await screen.findByRole('button', { name: 'Conversation Assistant' });
    await act(async () => { await router.navigate({ to: '/' }); });
    fireEvent.click(await screen.findByRole('button', {
      name: 'Conversation Assistant, on Test wave',
    }));
    await screen.findByRole('complementary', { name: 'Assistant' });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w1`);
  });

  /*
   * The same trip with the list arriving late, which is the shape the request
   * really has: the reader is navigated to the wave and the conversation list
   * is a round trip behind them. A consume that answers "not in the rows, so
   * clear it" is green on the test above and red here, and in production it
   * loses the request for good — the reader lands on the wave with the drawer
   * shut and nothing to press.
   */
  it('[G6] keeps the open request until the list it names arrives', async () => {
    let releaseList!: () => void;
    const listed = new Promise<void>((resolve) => { releaseList = resolve; });
    let holdList = false;
    const { client, router } = setup(async (request) => {
      if (request.path === CONVERSATIONS && holdList) {
        await listed;
        return ok([assistantRow()]);
      }
      return undefined;
    });
    await screen.findByRole('button', { name: 'Conversation Assistant' });
    await act(async () => { await router.navigate({ to: '/' }); });
    await screen.findByRole('button', { name: 'Conversation Assistant, on Test wave' });

    holdList = true;
    /* Dropped from the cache, or the second visit would render the first
       visit's rows and the request would be consumed before this test's
       question — "what happens while the list is in flight?" — could be asked
       at all. */
    client.removeQueries({ queryKey: queryKeys.waveConversations(WAVE.id) });
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Assistant, on Test wave' }));
    /*
     * On the wave with the list still in flight — and the wave's *detail*
     * already settled, which is the state that matters: that is precisely when
     * `WaveRoute` gets to say whether this request is one of its own. A route
     * that answers "no" here throws the request away a moment before the list
     * that would have opened it lands.
     */
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w1`));
    await waitFor(() => expect(client.isFetching({ queryKey: queryKeys.waveDetail(WAVE.id) })).toBe(0));
    /* The panel is up, so the route really has rendered against that detail. */
    await screen.findByRole('button', { name: 'New conversation' });
    expect(document.querySelector('[data-nc-drawer]')).toBeNull();

    await act(async () => { releaseList(); await Promise.resolve(); });
    await screen.findByRole('complementary', { name: 'Assistant' });
  });

  /*
   * The fallback the two effects above deliberately do not cover: the card is
   * on this wave, so nothing clears the request, and the list that would open
   * it could not be read, so nothing consumes it. Without a rule for this the
   * id sits in the registry for the life of the tab and the drawer springs open
   * the next time the reader wanders in.
   */
  it('[G6] gives up an open request whose list could not be read', async () => {
    let failList = false;
    const { client, router } = setup((request) => request.path === CONVERSATIONS && failList
      ? { status: 500, statusText: 'Error', body: { code: 'internal', error: 'boom' } }
      : undefined);
    await screen.findByRole('button', { name: 'Conversation Assistant' });
    await act(async () => { await router.navigate({ to: '/' }); });
    failList = true;
    client.removeQueries({ queryKey: queryKeys.waveConversations(WAVE.id) });
    fireEvent.click(await screen.findByRole('button', {
      name: 'Conversation Assistant, on Test wave',
    }));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w1`));
    /* Nothing opened — and, once the list recovers, still nothing opens: the
       request was given up rather than left lying around. */
    await waitFor(() => expect(screen.queryByRole('complementary', { name: 'Assistant' })).toBeNull());
    failList = false;
    await act(async () => { await router.navigate({ to: '/' }); });
    await act(async () => { await router.navigate({ to: '/wave/w1' }); });
    await screen.findByRole('button', { name: 'Conversation Assistant' });
    expect(screen.queryByRole('complementary', { name: 'Assistant' })).toBeNull();
  });

  /*
   * ── G7 ─────────────────────────────────────────────────────────────────────
   *
   * A draft belongs to what it was written on, and each route posts to its own
   * scope: the wave's key and words never reach the cove endpoint, and the
   * cove's `+` opens a blank draft rather than the wave's failed one.
   *
   * **Where the mutation lands is not here**, and the first half of this test
   * in particular is not a gate on the `scopeId` guard: wave and cove are two
   * sibling route components, so the walk between them unmounts the panel and
   * the wave's draft is gone before the cove's `+` is ever pressed — dropping
   * the `scopeId` comparison from `heldIs` leaves that assertion green (the
   * cove's `held` is null on arrival either way). What survives one panel
   * *instance* serving two scopes is the cove → cove walk, where `CoveRoute` is
   * not remounted across a param change: `cove-conversation.test.tsx` holds it with
   * `keeps a failed draft to the cove it belongs to` and `leaves another cove's
   * draft alone when a late create finally succeeds`, and both go red when the
   * `scopeId` comparison is dropped. Wave and cove are two different route
   * components, so what this test pins is the layer above — that the two `+`s,
   * the two derivations and the two endpoints are wired to their own scope, and
   * a draft written on one does not surface on the other. Neither claim is
   * covered by the cove pair, and neither is idle: they are what a shared panel
   * or a copy-pasted `create` would break.
   */
  it('[G7] keeps a wave draft off a cove, and posts each to its own scope', async () => {
    const { requests, router } = setup((request) => request.method === 'POST'
      && request.path.endsWith('/conversations')
      ? { status: 500, statusText: 'Error', body: { code: 'internal', error: 'boom' } }
      : undefined);
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    await openDraft();
    await write('words for the wave');
    /* Failed, so the draft is kept with its key and its words — which is the
       only state in which it could leak onto another route. */
    await screen.findByRole('button', { name: 'Try again' });
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));

    await act(async () => { await router.navigate({ to: '/cove/c1' }); });
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/cove/c1`));
    await openDraft();
    expect(screen.queryByText('words for the wave')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();

    await write('words for the cove');
    const everyCreate = () => requests.filter((request) => request.method === 'POST'
      && request.path.endsWith('/conversations'));
    await waitFor(() => expect(everyCreate()).toHaveLength(2));
    const [first, second] = everyCreate();
    expect(first?.path).toBe(CONVERSATIONS);
    expect(second?.path).toBe('/api/coves/c1/conversations');
    expect(second?.body).toEqual({ text: 'words for the cove' });
    expect(second?.headers?.['Idempotency-Key']).not.toBe(first?.headers?.['Idempotency-Key']);
  });

  /*
   * ── §5.4 ───────────────────────────────────────────────────────────────────
   *
   * An assistant card is read in the drawer and draws nothing, so it is
   * headless — like the spec card and the report card. Registering the entry is
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
