// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { HARNESS_ITEMS_PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { queryKeys } from '../providers/queries.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = { id: 'w1', area_id: 'c1', title: 'Test wave', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
const CARD = { id: 'card-1', wave_id: 'w1', kind: 'codex', title: 'Spec chat', sort: 1, payload: { spec_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const WAVE_B = { ...WAVE, id: 'w2', title: 'Second wave', sort: 2 };
const CARD_B = { ...CARD, id: 'card-2', wave_id: 'w2', title: 'Second chat' };
const CARD_SAME_WAVE = { ...CARD, id: 'card-other', title: 'Other chat' };

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

function harnessRows(count: number) {
  return Array.from({ length: count }, (_, index) => ({
    id: index + 1, runtime_id: 'runtime', card_id: CARD.id, wave_id: WAVE.id, thread_id: 'thread',
    turn_id: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
    params: JSON.stringify({ item: { text: `reply ${index}` } }), created_at_ms: index + 1,
  }));
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
      if (request.path === '/api/areas/c1/waves') return ok([WAVE, WAVE_B]);
      if (request.path === '/api/overlays?entity_kind=wave') return ok([]);
      if (request.path === '/api/waves/w1') return ok({ wave: WAVE, cards: [CARD], overlays: [] });
      if (request.path === '/api/waves/w2') return ok({ wave: WAVE_B, cards: [CARD_B], overlays: [] });
      if (request.path.includes('/harness/items')) return ok([]);
      if (request.path.endsWith('/spec/run')) return ok({ card_id: CARD.id, runtime_id: 'runtime', phase: 'idle' });
      if (request.path.endsWith('/spec/input')) return ok({ card_id: CARD.id, runtime_id: 'runtime' });
      if (request.path.endsWith('/spec/interrupt')) return ok({ card_id: CARD.id, runtime_id: 'runtime', stopped: true });
      if (request.path === '/api/settings') return ok({});
      return ok([]);
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  render(<QueryClientProvider client={client}><ThemeProvider storage={themeStorage}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { client, requests, router };
}

async function openConversation() {
  fireEvent.click(await screen.findByRole('button', { name: /Conversation Spec chat/ }));
  await screen.findByRole('complementary', { name: 'Spec chat' });
}

/*
 * `combobox`, not `textbox`, since #1189.
 *
 * The wave route is a `'rows'` route now, which means the composer carries the
 * `/` command menu — and `useTriggerMenu` only emits the combobox role when a
 * trigger is really configured. The accessibility tree is where that difference
 * is honest, so the lookup follows it rather than hiding it behind a `*ByRole`
 * that would match either.
 */
function messageField(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

/*
 * A conversation that actually has a transcript. It used to exist because the
 * reset control was only offered when there was something to throw away; reset
 * is gone (#1139) and this survives because "the drawer over a non-empty
 * transcript" is still the state the destructive-control sweep below has to be
 * run against — an empty drawer proves nothing about what a full one offers.
 */
function setupWithTurns(reply?: Reply) {
  return setup(async (request) => await reply?.(request)
    ?? (request.path.includes('/harness/items') ? ok(harnessRows(1)) : undefined));
}

async function openConversationWithTurns() {
  await openConversation();
  await screen.findByText('reply 0');
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

/*
 * Drive the composer the way a person does.
 *
 * `fireEvent.change` cannot: Astryx's `ChatComposerInput` is a
 * `contenteditable` div with no value setter, so `change` throws — and there is
 * no `<form>` to submit either, because `ChatComposer` is a div that sends on a
 * bare `Enter` keydown. So the text is written into the editable and an `input`
 * event fires (which is what feeds the field's React state), and Enter sends.
 */
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

async function sendWithEnter(field: HTMLElement) {
  await act(async () => {
    fireEvent.keyDown(field, { key: 'Enter' });
    await Promise.resolve();
  });
}

describe('spec conversation regressions', () => {
  /*
   * Three tests used to stand here, all of them about *when* the reset control
   * appeared: it had to be labelled rather than a glyph, it had to be absent
   * over a zero-turn conversation, and it had to arrive with the first turn.
   * The control is gone (#1139), so none of the three has a subject.
   *
   * What replaces them is a **set equality over every control the drawer
   * offers**, and the reason it is stated that way is worth recording, because
   * the obvious replacement does not work.
   *
   * The obvious one was `querySelectorAll('[data-nc-action="destructive"]')`,
   * on the reasoning that it catches the class of control rather than one
   * label. It catches nothing: the removed reset was a `DrawerAction`, which
   * renders `data-nc-role="icon"` plus a CSS-module `actionDanger` class and
   * has **never** carried `data-nc-action="destructive"`. That sweep was green
   * on `origin/main` with the reset button on screen — zero coverage of the
   * exact shape it claimed to fence, and the `/reset/i` query it disparaged in
   * prose was the only line in the test doing any work.
   *
   * So the fence is: these two buttons, and nothing else. It is red if the old
   * control comes back verbatim, red if it comes back under a new label or a
   * new glyph, red if it comes back with no `data-nc-action` at all — and red
   * for any *other* unannounced control that grows in here, which is more than
   * was asked for and is the right amount. Adding a control to this drawer is a
   * decision that should have to be written down; this is where it gets
   * written.
   *
   * The name used is the accessible name, because that is what a reader
   * actually meets. The two legitimate members are the close chevron and the
   * composer's Send.
   *
   * It is run over the *non-empty* drawer on purpose. The removed control was
   * conditional on there being a transcript, so an empty drawer is precisely
   * the state that never had one — proving nothing.
   */
  it('offers exactly the close and Send, and no other control at all', async () => {
    setupWithTurns();
    await openConversationWithTurns();
    const drawer = screen.getByRole('complementary', { name: 'Spec chat' });
    const names = within(drawer)
      .getAllByRole('button', { hidden: true })
      .map((button) => button.getAttribute('aria-label') ?? button.textContent);
    expect([...names].sort()).toEqual(['Close conversation', 'Send']);
    expect(screen.queryByRole('button', { name: /reset/i })).toBeNull();
  });

  /*
   * And the browser never calls the endpoint. The server still serves
   * `POST /spec/reset`; this pins that the front end has no path to it — a
   * UI-only removal that left a live caller wired to some other control would
   * be invisible to the set-equality above, which only reads the tree.
   *
   * This version **presses things**. The one it replaces opened the drawer,
   * clicked the wordmark, and asserted no reset POST — so the only caller it
   * could ever have caught was one that fired on mount by itself. Every control
   * the drawer offers is now pressed, the composer sends a message, and Escape
   * closes it, and none of that reaches the endpoint.
   */
  it('never posts to the spec reset endpoint, however the drawer is driven', async () => {
    const { requests } = setupWithTurns();
    await openConversationWithTurns();
    const drawer = screen.getByRole('complementary', { name: 'Spec chat' });

    const field = within(drawer).getByRole('combobox', { name: 'Message' });
    await typeInto(field, 'a message');
    await sendWithEnter(field);

    /* Every button in the drawer, in tree order, ending on the close — reversed
       so the close is pressed last and the rest are pressed while the drawer is
       still up. */
    const controls = within(drawer)
      .getAllByRole('button', { hidden: true })
      .filter((button) => button.getAttribute('aria-label') !== 'Close conversation');
    for (const control of controls) fireEvent.click(control);
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.click(within(drawer).getByRole('button', { name: 'Close conversation' }));

    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    await screen.findByRole('button', { name: /Conversation Spec chat, on Test wave/ });
    expect(requests.filter((request) => request.path.endsWith('/spec/reset'))).toHaveLength(0);
    /* And the pressing above actually did something, so an inert sweep cannot
       pass this by touching nothing. */
    expect(requests.filter((request) => request.path.endsWith('/spec/input'))).toHaveLength(1);
  });

  /*
   * The wave route used to be the one route with a `+` and no `/new`: it had
   * exactly one spec card, `start()` reopened the row already open, and a
   * command named `New conversation` that reopens the conversation you are
   * reading is a lie told by a control.
   *
   * #1189 removed the premise. A wave holds as many assistant conversations as
   * you start, so `/new` here means what it means on an area, and the composer
   * becomes the combobox `useTriggerMenu` emits when a trigger is configured.
   * Asserted through the accessibility tree, which is where the difference is
   * visible to a reader.
   */
  it('offers /new in the wave composer, now that a wave can hold a second conversation', async () => {
    setupWithTurns();
    await openConversationWithTurns();
    const field = messageField();
    expect(field.getAttribute('aria-haspopup')).toBe('listbox');
    expect(screen.queryByRole('textbox', { name: 'Message' })).toBeNull();
  });

  /*
   * No turn count on these labels since #1189: a wave route lists rows, and a
   * row it has not opened is one it cannot count the turns of. `ChatList` says
   * nothing rather than `0 turns`, which would be a claim. The open row still
   * counts, because the drawer is reading its transcript — that is what the
   * Today test below asserts.
   */
  it('keeps a wave route conversation list scoped after visiting another wave', async () => {
    const { router } = setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    await router.navigate({ to: '/wave/w2' });
    await screen.findByRole('button', { name: 'Conversation Second chat' });
    expect(screen.queryByRole('button', { name: 'Conversation Spec chat' })).toBeNull();
  });

  it('keeps a wave conversation on Today after navigating away from the wave', async () => {
    setup();
    await openConversation();
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    const conversation = await screen.findByRole('button', {
      name: 'Conversation Spec chat, on Test wave, 0 turns',
    });
    expect(conversation.textContent).toContain('Test wave');
  });

  /*
   * #1189 §5.1 / G5 — and the point is that the wave was only *visited*.
   *
   * The row was never opened, so nothing here went through the open-row
   * remember: the wave route writes the rows it lists into the registry as it
   * lists them, which is the only way an assistant conversation — which exists
   * nowhere but in that list — can ever reach Today.
   */
  it('lists a wave conversation on Today after merely visiting the wave', async () => {
    const { router } = setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    await router.navigate({ to: '/' });
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave' });
  });

  it('navigates from a Today conversation to its wave before opening it', async () => {
    const { requests } = setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    fireEvent.click(await screen.findByRole('button', {
      name: 'Conversation Spec chat, on Test wave',
    }));
    await screen.findByRole('complementary', { name: 'Spec chat' });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w1`);
    expect(requests.some(({ path }) => path.includes('/api/cards//'))).toBe(false);
  });

  /*
   * An area lists its *own* conversations, from the server (#1098) — never the
   * spec conversations of the waves inside it. Those hang off a wave and are
   * read on that wave's page; listing them here would put rows in a panel whose
   * drawer this route deliberately opens in place, on a card it has no scope
   * for. Today is still where a remembered wave conversation shows up.
   */
  it('does not list a wave spec conversation on that wave\'s area', async () => {
    setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    fireEvent.click(screen.getByRole('button', { name: 'Work' }));
    await screen.findByText('No conversations yet.');
    expect(screen.queryByRole('button', { name: /Conversation Spec chat/ })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave' });
  });

  /*
   * Three "after reset …" registry tests stood here. All three drove the same
   * machinery — `suppressRememberRef` / `suppressedRememberSnapshotRef` in
   * `useConversationStore`, which existed *only* to stop a stale pre-reset
   * snapshot being written back into the session registry while the
   * invalidation raced. Reset was that mechanism's one and only writer, so the
   * fields are deleted with it and there is nothing left to suppress.
   *
   * What was worth keeping from them is the part that is not about reset: the
   * registry must still track a card through a card switch and still carry the
   * conversation to Today. That is what the test below now does, driven by the
   * card list changing under the route rather than by a reset.
   */
  it('remembers a card again after the open card is swapped and swapped back', async () => {
    const { client, router } = setup((request) => request.path.includes('/harness/items')
      ? ok(harnessRows(3)) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Spec chat' }));
    /* The open row is the one this route can count, so it is the one that grows
       a turn count — and waiting for it is also how we know the transcript has
       arrived before the card is swapped underneath it. */
    await screen.findByRole('button', { name: 'Conversation Spec chat, 3 turns' });

    client.setQueryData(queryKeys.harnessItems(CARD_SAME_WAVE.id), {
      pages: [harnessRows(3)], pageParams: [0],
    });
    client.setQueryData(queryKeys.waveDetail(WAVE.id), { wave: WAVE, cards: [CARD_SAME_WAVE], overlays: [] });
    /* The listed row is the swapped-in card, and the drawer's row is gone with
       the old one — a `'rows'` route lists what the server (here, the wave
       detail) says, so the count only comes back when this one is opened. */
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Other chat' }));
    await screen.findByRole('button', { name: 'Conversation Other chat, 3 turns' });
    client.setQueryData(queryKeys.waveDetail(WAVE.id), { wave: WAVE, cards: [CARD], overlays: [] });
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    await router.navigate({ to: '/' });
    /* Both are on Today, and the first one still carries the transcript it was
       remembered with — the swap did not cost it. */
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave, 3 turns' });
    await screen.findByRole('button', { name: 'Conversation Other chat, on Test wave, 3 turns' });
  });

  it('clears an unclaimed open request after a wave without a spec card resolves', async () => {
    let omitTargetCard = false;
    const { client, router } = setup((request) => {
      if (request.path === '/api/waves/w1' && omitTargetCard) {
        return ok({ wave: WAVE, cards: [], overlays: [] });
      }
      if (request.path === '/api/waves/w2') {
        return ok({ wave: WAVE_B, cards: [{ ...CARD, wave_id: WAVE_B.id }], overlays: [] });
      }
      return undefined;
    });
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    await router.navigate({ to: '/' });
    omitTargetCard = true;
    client.removeQueries({ queryKey: queryKeys.waveDetail(WAVE.id) });
    fireEvent.click(await screen.findByRole('button', {
      name: 'Conversation Spec chat, on Test wave',
    }));
    await screen.findByText('No cards yet.');
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();

    await router.navigate({ to: '/wave/w2' });
    await screen.findByRole('button', { name: 'Conversation Spec chat' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();
  });

  it('renders a server-sent reply from the history fixture', async () => {
    setup((request) => request.path.includes('/harness/items') ? ok(harnessRows(1)) : undefined);
    await openConversation();
    expect(await screen.findByText('reply 0')).toBeTruthy();
  });

  it('keeps a completed action in its started position', async () => {
    const rows = [
      {
        ...harnessRows(1)[0], id: 1, item_uuid: 'command-1', item_type: 'commandExecution',
        method: 'item/started', params: JSON.stringify({ item: { command: 'npm test' } }),
      },
      {
        ...harnessRows(1)[0], id: 2, item_uuid: 'message-1',
        params: JSON.stringify({ completedAtMs: 20, item: { text: 'interleaved reply' } }),
      },
      {
        ...harnessRows(1)[0], id: 3, item_uuid: 'command-1', item_type: 'commandExecution',
        params: JSON.stringify({ completedAtMs: 30, item: { command: 'npm test', exitCode: 0 } }),
      },
    ];
    setup((request) => request.path.includes('/harness/items') ? ok(rows) : undefined);
    await openConversation();
    /* The history is fetched when the row is *opened* now — the wave route no
       longer holds a card scope before that — so the transcript lands a round
       trip after the drawer does. */
    await screen.findByText('interleaved reply');
    const drawer = screen.getByRole('complementary', { name: 'Spec chat' });
    const actionIndex = drawer.textContent?.indexOf('Ran') ?? -1;
    const replyIndex = drawer.textContent?.indexOf('interleaved reply') ?? -1;
    expect(actionIndex).toBeGreaterThanOrEqual(0);
    expect(replyIndex).toBeGreaterThanOrEqual(0);
    expect(actionIndex).toBeLessThan(replyIndex);
  });

  it('drops a completed tail thought when an optimistic message follows it', async () => {
    const thought = {
      ...harnessRows(1)[0], item_uuid: 'thought-1', item_type: 'reasoning',
      params: JSON.stringify({ item: { summary: [] } }),
    };
    setup((request) => request.path.includes('/harness/items') ? ok([thought]) : undefined);
    await openConversation();
    expect(await screen.findByText('Thought')).toBeTruthy();
    const field = messageField();
    await typeInto(field, 'next message');
    await sendWithEnter(field);
    expect(await screen.findByText('next message')).toBeTruthy();
    expect(screen.queryByText('Thought')).toBeNull();
  });

  it('loads only the first history page until the user asks for earlier rows', async () => {
    const { requests } = setup((request) => request.path.includes('/harness/items')
      ? ok(harnessRows(HARNESS_ITEMS_PAGE_LIMIT)) : undefined);
    await openConversation();
    const historyRequests = () => requests.filter((request) => request.path.includes('/harness/items'));
    await waitFor(() => expect(historyRequests()).toHaveLength(1));
    fireEvent.click(screen.getByRole('button', { name: 'Load earlier' }));
    await waitFor(() => expect(historyRequests()).toHaveLength(2));
  });

  it('surfaces send failures and prevents a second send while the first is pending', async () => {
    let reject!: (reason: Error) => void;
    const pending = new Promise<ApiTransportResponse>((_resolve, rejectPromise) => { reject = rejectPromise; });
    const { requests } = setup((request) => request.path.endsWith('/spec/input') ? pending : undefined);
    await openConversation();
    const field = messageField();
    await typeInto(field, 'hello');
    await sendWithEnter(field);
    await sendWithEnter(field);
    expect(requests.filter((request) => request.path.endsWith('/spec/input'))).toHaveLength(1);
    reject(new Error('send exploded'));
    expect((await screen.findByRole('alert')).textContent).toContain('Transport request failed');
  });

  it('invalidates history and phase after a successful send', async () => {
    const { requests } = setup();
    await openConversation();
    await waitFor(() => {
      expect(requests.filter((request) => request.path.includes('/harness/items')).length).toBeGreaterThan(0);
      expect(requests.filter((request) => request.path.endsWith('/spec/run')).length).toBeGreaterThan(0);
    });
    const beforeHistory = requests.filter((request) => request.path.includes('/harness/items')).length;
    const beforeRun = requests.filter((request) => request.path.endsWith('/spec/run')).length;
    const field = messageField();
    await typeInto(field, 'hello');
    await sendWithEnter(field);
    await waitFor(() => {
      expect(requests.filter((request) => request.path.includes('/harness/items'))).toHaveLength(beforeHistory + 1);
      expect(requests.filter((request) => request.path.endsWith('/spec/run'))).toHaveLength(beforeRun + 1);
    });
  });

  /*
   * Two reset-failure tests stood here — one pinning "one POST for two
   * confirmations, and the rejection surfaced", the other "a turn count that
   * moved while the reset was in flight is the one Today keeps". Both are
   * gone with the action.
   *
   * The first one's non-reset half survives above, in `surfaces send failures
   * and prevents a second send while the first is pending`: same shape (a
   * pending request, a double press, one call, the error surfaced) on the one
   * mutation the drawer still has. The second's survives in the registry test
   * further up, which is now driven by the card list rather than by a reset.
   */

  it('uses Escape to interrupt a working turn without closing the drawer', async () => {
    let resolveInterrupt!: (response: ApiTransportResponse) => void;
    const pendingInterrupt = new Promise<ApiTransportResponse>((resolve) => { resolveInterrupt = resolve; });
    const { requests } = setup((request) => {
      if (request.path.endsWith('/spec/run')) {
        return ok({ card_id: CARD.id, runtime_id: 'runtime', phase: 'turn_running' });
      }
      return request.path.endsWith('/spec/interrupt') ? pendingInterrupt : undefined;
    });
    await openConversation();
    const drawer = screen.getByRole('complementary', { name: 'Spec chat' });
    /* The phase query starts with the drawer, so the turn is only known to be
       running a round trip later — and Escape does nothing until it is. */
    await waitFor(() => expect(requests.some((request) => request.path.endsWith('/spec/run'))).toBe(true));
    await waitFor(() => {
      fireEvent.keyDown(drawer, { key: 'Escape' });
      expect(requests.filter((request) => request.path.endsWith('/spec/interrupt'))).toHaveLength(1);
    });
    fireEvent.keyDown(drawer, { key: 'Escape' });
    expect(requests.filter((request) => request.path.endsWith('/spec/interrupt'))).toHaveLength(1);
    expect(screen.getByRole('complementary', { name: 'Spec chat' })).toBeTruthy();
    resolveInterrupt(ok({ card_id: CARD.id, runtime_id: 'runtime', stopped: true }));
  });

  /*
   * `cancels reset on Escape without also closing the drawer` stood here. The
   * Escape layering it protected — an inner surface eats the key before the
   * drawer does — is unchanged and still needs a guard; its new subject is the
   * `/` command menu, and that test lives in `area-conversation.test.tsx`
   * beside the rest of the slash-command behaviour, because the wave route
   * deliberately has no `/` menu (see `startAnother` in the router).
   */
});
