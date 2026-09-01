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

import { useEffect } from 'react';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { waveConversationCardId } from '../../../../core/domain/conversation.ts';
import { ConversationProvider, useConversationRegistry } from '../conversations/public.tsx';
import { queryKeys } from '../providers/queries.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter, useConversationStore } from './public.tsx';
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

/**
 * One persisted transcript row, in the shape the harness serves.
 *
 * The two item types do not carry their text the same way — an agent message
 * has `text`, a user message has `content` parts (`harnessItemToTurn`) — so the
 * payload is the caller's to spell, and a row spelled the other way silently
 * yields no turn at all.
 */
function harnessMessage(id: number, itemType: string, item: unknown) {
  return {
    id, runtime_id: 'r', card_id: ASSISTANT_CARD.id, wave_id: 'w1', thread_id: 't',
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
      /* Both card endpoints echo **the card in the path**, which is what the
         kernel does (`cards.rs`'s `/spec/input` answers with the card it just
         accepted input for). A fixture answering with a fixed id is a server
         that does not exist: nothing here reads the field today, so it is not
         a false green yet — it is a trap laid for the first case that does,
         which would then pass against a reply about the wrong conversation. */
      if (request.path.endsWith('/spec/run')) return ok({ card_id: pathCardId(request.path), runtime_id: 'r', phase: 'idle' });
      /* Answering an open conversation's send. The shape matters: an
         off-schema body is refused by the transport, the optimistic echo is
         rolled back, and the name derived from that echo — what the test
         below is about — never exists. */
      if (request.path.endsWith('/spec/input')) return ok({ card_id: pathCardId(request.path), runtime_id: 'r' });
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
   * And the name it did **not** earn, which is the same carry-over read from
   * the other side.
   *
   * An echo goes on the screen the instant Enter is pressed, named and timed
   * from the browser's own clock, and it is not yet a fact — the POST can still
   * fail. Two rules that are each right compose into a wrong one: the effect
   * above remembers the open conversation, and the batch remember below the
   * drawer carries an entry's name and time forward rather than letting a
   * `title: null` row undo them. Close the drawer while the POST is in flight
   * and the optimistic values are what get carried; when the POST then fails,
   * `scope` is already null, so the `catch` that drops the echo reaches nothing
   * and the registry — which has no `forget` — keeps them for the life of the
   * tab. Today was left naming a conversation after a message that never left
   * the browser, and holding it above rows that really were touched.
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
    /* A row this tab has genuinely just seen activity on, timed a second ago on
       the same clock the echo would use. It is what makes the *time* half of
       this assertable at all: `updatedAt: 30` sorts below everything, so a row
       carrying a leaked `Date.now()` and a row carrying its own honest time are
       in the same place on Today unless something recent sits between them. */
    const touchedAt = Date.now() - 1000;
    const { router } = setup(async (request) => {
      if (request.path === CONVERSATIONS) {
        return ok([assistantRow(), assistantRow({
          id: 'conv-assistant-2', title: 'Recently touched', updatedAt: touchedAt,
        })]);
      }
      if (request.path.endsWith('/spec/input')) {
        await settled;
        return { status: 503, statusText: 'Service Unavailable', body: { code: 'unavailable', error: 'busy' } };
      }
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('complementary', { name: 'Assistant' });
    await write('a message that never lands');
    /* The premise, and the reason this bug was invisible: the optimistic name
       really is derived and really is on screen. The drawer is right to show
       it — you did type it — and that is exactly why the registry must not
       take it. */
    await screen.findByRole('complementary', { name: 'a message that never lands' });

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    /*
     * Shut, with the POST still out: this is the window, and releasing the
     * rejection before this point would test a different code path.
     *
     * "Shut" is read off the panel rather than off `[data-nc-drawer]`, which
     * stays in the document for one exit animation and therefore forever under
     * jsdom (`ui/drawer`: the element is held mounted until `animationend`).
     * The list's `aria-current` is the route's own answer to "is a conversation
     * open", and it is exactly the state the store reads — `scope` is null the
     * moment no row is current, which is what stops the effects.
     */
    await waitFor(() => expect(
      document.querySelector('[data-nc-role="row"][aria-current="true"]'),
    ).toBeNull());
    await act(async () => { rejectInput(); await Promise.resolve(); });

    await act(async () => { await router.navigate({ to: '/' }); });
    const rows = () => screen.getAllByRole('button', { name: /^Conversation / })
      .map((row) => row.getAttribute('aria-label') ?? '');
    /* The kind label, because there is still no name — not the sentence that
       was never sent. `0 turns` is the same fact said twice: the count is the
       length of the confirmed turns, and a conversation whose only message
       failed has had none. */
    await screen.findByRole('button', {
      name: 'Conversation Assistant, on Test wave, 0 turns',
    });
    expect(screen.queryByRole('button', { name: /never lands/ })).toBeNull();
    /* And it did not float to the top on a clock only this browser read: the
       row that really was touched a second ago is still above it. */
    const listed = rows();
    expect(listed.indexOf('Conversation Recently touched, on Test wave'))
      .toBeLessThan(listed.indexOf('Conversation Assistant, on Test wave, 0 turns'));
  });

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
      if (!request.path.endsWith('/spec/input')) return undefined;
      const cardId = pathCardId(request.path);
      await new Promise<void>((resolve) => { held.set(cardId, resolve); });
      return { status: 503, statusText: 'Service Unavailable', body: { code: 'unavailable', error: 'busy' } };
    });
    /* Conversation A, one message, request still out. */
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('complementary', { name: 'Assistant' });
    await write('the first conversation speaks');
    await waitFor(() => expect(held.has(ASSISTANT_CARD.id)).toBe(true));

    /* Conversation B, on the same panel instance — the walk that resets
       `sendingRef` so this composer works at all. */
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: 'Conversation Spec chat' }));
    await screen.findByRole('complementary', { name: 'Spec chat' });
    await write('the second conversation speaks');
    await waitFor(() => expect(held.has(SPEC_CARD.id)).toBe(true));
    const sends = () => requests.filter((request) =>
      request.path === `/api/cards/${SPEC_CARD.id}/spec/input`);
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
   * The other side of the write-through: it may not undo what arrived while it
   * was waiting.
   *
   * `mutations.send` resolves two refreshes *after* its POST returned 200
   * (`useSpecMutations` invalidates the item history and the run), and either
   * refresh can land first. So the interval between "this message is a fact"
   * and "the store may say so" is one in which the entry legitimately changes:
   * the server's own copy of the message arrives, the agent's reply arrives
   * with it, the effects write both into the registry — and then the drawer is
   * closed and the second refresh finally settles.
   *
   * A callback merging into the list it captured when Enter was pressed writes
   * the pre-send entry back: the reply is gone, the count is the one this
   * browser could count on its own, and nothing will ever fetch them again for
   * a conversation whose only surface is a wave the reader has left. The read
   * and the write have to be the same moment, which is `updateExisting`.
   *
   * `2 turns` is the assertion, and it is one number that rejects both ways of
   * getting this wrong: a captured snapshot says `1` (the echo, on top of an
   * entry that knew nothing), and an atomic merge that appends its echo without
   * noticing the server already sent that message back says `3`.
   */
  it('[G5] does not overwrite a refresh that landed while the send was still settling', async () => {
    let releaseRun!: () => void;
    const runSettled = new Promise<void>((resolve) => { releaseRun = resolve; });
    let answered = false;
    const { router } = setup(async (request) => {
      if (request.path.startsWith(`/api/cards/${ASSISTANT_CARD.id}/harness/items`)) {
        return ok(answered ? [
          harnessMessage(1, 'userMessage', { content: [{ text: 'what does this repo do?' }] }),
          harnessMessage(2, 'agentMessage', { text: 'it runs waves' }),
        ] : []);
      }
      if (request.path === `/api/cards/${ASSISTANT_CARD.id}/spec/input`) {
        answered = true;
        return ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r' });
      }
      /* The *second* of the two refreshes the send waits on, held open. The
         first one — the history — is answered above and is what lands the new
         facts in the registry while this one is still out. */
      if (answered && request.path === `/api/cards/${ASSISTANT_CARD.id}/spec/run`) {
        await runSettled;
        return ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r', phase: 'idle' });
      }
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    await screen.findByRole('complementary', { name: 'Assistant' });
    await write('what does this repo do?');
    /* The window is open: the history refresh has landed — the agent's reply is
       on screen and in the registry — and the send has not settled. */
    await screen.findByText('it runs waves');

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    await waitFor(() => expect(
      document.querySelector('[data-nc-role="row"][aria-current="true"]'),
    ).toBeNull());
    await act(async () => { releaseRun(); await Promise.resolve(); });

    await act(async () => { await router.navigate({ to: '/' }); });
    await screen.findByRole('button', {
      name: 'Conversation what does this repo do?, on Test wave, 2 turns',
    });
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
   * mint, calls this one already recorded, and neither appends it nor counts
   * it. `reconcileUserEchoes` pairs one-to-one only among the echoes of a
   * single call, and this call passes one, so nothing tells it the old row is
   * already spoken for. Only the rows that arrived *since the send* can answer
   * the question that was asked.
   *
   * `2 turns` is the assertion and it fails in the one direction that matters:
   * the old-`ping`-answers-for-the-new bug reports `1`.
   */
  it('[G5] counts a message really sent twice, when the refresh is a moment behind', async () => {
    let releaseRun!: () => void;
    const runSettled = new Promise<void>((resolve) => { releaseRun = resolve; });
    let answered = false;
    const { router } = setup(async (request) => {
      /* The server's copy of the *first* `ping`, and only ever that one: the
         second is accepted and persisted, and this read cannot see it yet. */
      if (request.path.startsWith(`/api/cards/${ASSISTANT_CARD.id}/harness/items`)) {
        return ok([harnessMessage(1, 'userMessage', { content: [{ text: 'ping' }] })]);
      }
      if (request.path === `/api/cards/${ASSISTANT_CARD.id}/spec/input`) {
        answered = true;
        return ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r' });
      }
      /* Held so the send is still settling when the drawer shuts, which is the
         only window in which the write-through decides anything. */
      if (answered && request.path === `/api/cards/${ASSISTANT_CARD.id}/spec/run`) {
        await runSettled;
        return ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r', phase: 'idle' });
      }
      return undefined;
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Assistant' }));
    /* Named from the `ping` that is already there — the row this test is about
       having to not answer for the next one. */
    await screen.findByRole('complementary', { name: 'ping' });
    await write('ping');

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    await waitFor(() => expect(
      document.querySelector('[data-nc-role="row"][aria-current="true"]'),
    ).toBeNull());
    await act(async () => { releaseRun(); await Promise.resolve(); });

    await act(async () => { await router.navigate({ to: '/' }); });
    await screen.findByRole('button', {
      name: 'Conversation ping, on Test wave, 2 turns',
    });
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

/*
 * ── Echo identity, which outlives the store that minted it ───────────────────
 *
 * An echo id is minted off a counter in `useConversationStore`, and that store
 * dies with the route. The registry does not: it hangs off `ConversationProvider`
 * at the root and is the one thing on this surface with no `forget`. Leave a
 * wave with a send still in flight and come back, and a *second* store starts
 * counting from one while the first one's request is still out — two different
 * messages arriving in one shared entry under one id.
 *
 * Driven through the store itself rather than the router because the defect is
 * an identity, and the routes render turns by that identity without ever showing
 * it: the count is `2` either way, and what a duplicate id costs — a React key
 * that names two rows, a jump target that resolves to the wrong one — is only
 * visible where the ids are. The two stores are mounted and unmounted for real
 * under one provider, so the lifetimes being claimed are the real ones.
 */
describe('echo identity across store instances', () => {
  const SCOPE = {
    id: 'w1', title: 'Test wave', cardId: ASSISTANT_CARD.id, cardTitle: null,
    updatedAt: 30, kind: 'wave-assistant' as const, state: 'idle' as const,
  };
  const ROWS: readonly Conversation[] = [{
    id: ASSISTANT_CARD.id, waveId: 'w1', waveTitle: 'Test wave', title: null,
    kind: 'wave-assistant', state: 'idle', updatedAt: 30,
  }];

  it('[G5] mints echo ids that a later mount cannot collide with', async () => {
    /* Every send is held, so both are still out when their stores unmount. */
    const holds: (() => void)[] = [];
    const transport: ApiTransportPort = {
      async send(request) {
        if (request.path.endsWith('/spec/input')) {
          await new Promise<void>((resolve) => { holds.push(resolve); });
          return ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r' });
        }
        if (request.path.endsWith('/spec/run')) {
          return ok({ card_id: ASSISTANT_CARD.id, runtime_id: 'r', phase: 'idle' });
        }
        /* No history: the server has brought nothing back, which is the state
           in which an echo is the only account of the message. */
        return ok([]);
      },
    };
    let latestSend: (text: string) => void = () => undefined;
    let latestTurns: readonly TranscriptEntry[] = [];

    function StoreProbe() {
      const store = useConversationStore(transport, unauthorized, SCOPE, {
        kind: 'rows', rows: ROWS, rememberOn: 'w1',
      });
      const send = store.send;
      useEffect(() => { latestSend = (text) => { send(ASSISTANT_CARD.id, text); }; });
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

    await act(async () => { latestSend('first'); await Promise.resolve(); });
    await waitFor(() => expect(holds).toHaveLength(1));
    /* The walk away: this store is gone, its counter with it, and its request
       is still out. */
    await act(async () => { rerender(view(null)); await Promise.resolve(); });
    /* And the walk back — a new store, on the same conversation, under the same
       registry. */
    await act(async () => { rerender(view('second-mount')); await Promise.resolve(); });
    await act(async () => { latestSend('second'); await Promise.resolve(); });
    await waitFor(() => expect(holds).toHaveLength(2));
    await act(async () => { rerender(view(null)); await Promise.resolve(); });

    /* Both answers land with nobody mounted, which is exactly when the
       write-through is the only writer of this entry. */
    await act(async () => {
      for (const release of holds) release();
      await Promise.resolve();
    });
    await waitFor(() => expect(latestTurns).toHaveLength(2));
    expect(latestTurns.map((turn) => 'text' in turn ? turn.text : '')).toEqual(['first', 'second']);
    /* The assertion: two messages, two identities. A counter reset by the
       remount hands the registry both of them as `echo-1`. */
    expect(new Set(latestTurns.map((turn) => turn.id)).size).toBe(2);
  });
});
