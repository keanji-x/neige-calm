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
const TRACK = { id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
const CARD = { id: 'card-1', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1, payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const TRACK_B = { ...TRACK, id: 'w2', title: 'Second track', sort: 2 };
const CARD_B = { ...CARD, id: 'card-2', track_id: 'w2', title: 'Second chat' };
const CARD_SAME_TRACK = { ...CARD, id: 'card-other', title: 'Other chat' };
const PLANNER_RUN_IDLE = { card_id: CARD.id, worker_session_id: 'runtime', phase: 'idle' };

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

function harnessRows(count: number) {
  return Array.from({ length: count }, (_, index) => ({
    id: index + 1, worker_session_id: 'runtime', card_id: CARD.id, track_id: TRACK.id, thread_id: 'thread',
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
      if (request.path === '/api/areas/c1/tracks') return ok([TRACK, TRACK_B]);
      if (request.path === '/api/overlays?entity_kind=track') return ok([]);
      if (request.path === '/api/tracks/w1') return ok({
        track: TRACK, can_resume: false, cards: [CARD], overlays: [],
      });
      if (request.path === '/api/tracks/w2') return ok({
        track: TRACK_B, can_resume: false, cards: [CARD_B], overlays: [],
      });
      if (request.path.includes('/harness/items')) return ok([]);
      if (request.path.endsWith('/planner/run')) return ok(PLANNER_RUN_IDLE);
      if (request.path.endsWith('/planner/input')) return ok({ card_id: CARD.id, worker_session_id: 'runtime' });
      if (request.path.endsWith('/planner/interrupt')) return ok({ card_id: CARD.id, worker_session_id: 'runtime', stopped: true });
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
  fireEvent.click(await screen.findByRole('button', { name: /Conversation Planner chat/ }));
  await screen.findByRole('complementary', { name: 'Planner chat' });
}

/*
 * `combobox`, not `textbox`, since #1189.
 *
 * The track route is a `'rows'` route now, which means the composer carries the
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
  window.history.pushState({}, '', `${APP_BASEPATH}/track/w1`);
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

describe('planner conversation regressions', () => {
  it('does not repeat the run phase above the composer while the planner is working', async () => {
    setupWithTurns((request) => request.path.endsWith('/planner/run')
      ? ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running' })
      : undefined);
    await openConversationWithTurns();
    expect(screen.queryByText(/Turn complete|Still working|Stopping this turn/)).toBeNull();
    expect(screen.getByRole('button', { name: 'Stop' })).toBeTruthy();
  });

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
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });
    const names = within(drawer)
      .getAllByRole('button', { hidden: true })
      .map((button) => button.getAttribute('aria-label') ?? button.textContent);
    expect([...names].sort()).toEqual(['Close conversation', 'Send']);
    expect(screen.queryByRole('button', { name: /reset/i })).toBeNull();
  });

  /*
   * And the browser never calls the endpoint. The server still serves
   * `POST /planner/reset`; this pins that the front end has no path to it — a
   * UI-only removal that left a live caller wired to some other control would
   * be invisible to the set-equality above, which only reads the tree.
   *
   * This version **presses things**. The one it replaces opened the drawer,
   * clicked the wordmark, and asserted no reset POST — so the only caller it
   * could ever have caught was one that fired on mount by itself. Every control
   * the drawer offers is now pressed, the composer sends a message, and Escape
   * closes it, and none of that reaches the endpoint.
   */
  it('never posts to the planner reset endpoint, however the drawer is driven', async () => {
    const { requests, router } = setupWithTurns();
    await openConversationWithTurns();
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });

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

    /* And once more from a fresh mount of the route, since a caller that fires
       on mount is the only kind the version this replaced could have caught.
       Today no longer lists another track's conversations (#1341), so it is a
       neutral mounted route to cross before returning here. */
    await act(async () => { await router.navigate({ to: '/' }); });
    await act(async () => { await router.navigate({ to: '/track/w1' }); });
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    expect(requests.filter((request) => request.path.endsWith('/planner/reset'))).toHaveLength(0);
    /* And the pressing above actually did something, so an inert sweep cannot
       pass this by touching nothing. */
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
  });

  /*
   * The track route used to be the one route with a `+` and no `/new`: it had
   * exactly one planner card, `start()` reopened the row already open, and a
   * command named `New conversation` that reopens the conversation you are
   * reading is a lie told by a control.
   *
   * #1189 removed the premise. A track holds as many assistant conversations as
   * you start, so `/new` here means what it means on an area, and the composer
   * becomes the combobox `useTriggerMenu` emits when a trigger is configured.
   * Asserted through the accessibility tree, which is where the difference is
   * visible to a reader.
   */
  it('offers /new in the track composer, now that a track can hold a second conversation', async () => {
    setupWithTurns();
    await openConversationWithTurns();
    const field = messageField();
    expect(field.getAttribute('aria-haspopup')).toBe('listbox');
    expect(screen.queryByRole('textbox', { name: 'Message' })).toBeNull();
  });

  /*
   * No turn count on these labels since #1189: a track route lists rows, and a
   * row it has not opened is one it cannot count the turns of. `ChatList` says
   * nothing rather than `0 turns`, which would be a claim. The open row still
   * counts, because the drawer is reading its transcript — that is what the
   * Today test below asserts.
   */
  it('keeps a track route conversation list scoped after visiting another track', async () => {
    const { router } = setup();
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await router.navigate({ to: '/track/w2' });
    await screen.findByRole('button', { name: 'Conversation Second chat' });
    expect(screen.queryByRole('button', { name: 'Conversation Planner chat' })).toBeNull();
  });

  /*
   * ── Three Today tests stood here, and #1341 revoked all three ──────────────
   *
   * They were `keeps a track conversation on Today after navigating away from
   * the track`, `lists a track conversation on Today after merely visiting the
   * track` and `navigates from a Today conversation to its track before opening
   * it` — the #1189 S5 deliverable that Today lists the tab's cross-track
   * visiting history and navigates into whichever track a row belongs to.
   *
   * Owner reversed the contract: Today lists the LAUNCHPAD track's own
   * conversations, by the same rule this route uses for itself, and it opens
   * them in place. A track other than the launchpad therefore contributes
   * nothing to Today however often it is visited, so all three describe a
   * behaviour that is not merely unimplemented but deliberately gone. The new
   * contract, including the inverse of these three, is
   * `today-conversation.test.tsx`; the cross-track index they were reaching for
   * becomes its own card on its own issue.
   *
   * One assertion from the third is not about Today and survived the move: that
   * no `/api/cards//` request is ever made — a card path built from an empty id.
   * It is asserted there, where the empty id is now possible (a workspace with
   * no launchpad yet).
   */

  /*
   * Three "after reset …" registry tests stood here. All three drove the same
   * machinery — `suppressRememberRef` / `suppressedRememberSnapshotRef` in
   * `useConversationStore`, which existed *only* to stop a stale pre-reset
   * snapshot being written back into the session registry while the
   * invalidation raced. Reset was that mechanism's one and only writer, so the
   * fields are deleted with it and there is nothing left to suppress.
   *
   * What was worth keeping from them is the part that is not about reset, and
   * #1341 narrowed even that — so the claim is worth stating exactly, because a
   * wider one was written here first and was not true.
   *
   * This asserts the **route**, not the registry: the list follows the track
   * detail through a card swap and back, and the row the drawer is on is
   * replaced in place by the counted one, twice, on one mounted panel. It used
   * to end on Today and read the registry's memory of the swapped-out card;
   * Today lists the launchpad's own conversations now (#1341) and this track is
   * not the launchpad, so that ending is gone. Stubbing `registry.remember` to a
   * no-op leaves this test green — which is the honest statement of its scope.
   * What the registry still holds, and who still reads it, is
   * `track-conversation.test.tsx`'s `registry write-through` block.
   */
  it('follows a card swapped out and back, counting whichever row is open', async () => {
    const { client } = setup((request) => request.path.includes('/harness/items')
      ? ok(harnessRows(3)) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Planner chat' }));
    /* The open row is the one this route can count, so it is the one that grows
       a turn count — and waiting for it is also how we know the transcript has
       arrived before the card is swapped underneath it. */
    await screen.findByRole('button', { name: 'Conversation Planner chat, 3 turns' });

    client.setQueryData(queryKeys.trackDetail(TRACK.id), {
      track: TRACK, can_resume: false, cards: [CARD_SAME_TRACK], overlays: [],
    });
    /* The listed row is the swapped-in card, and the drawer's row is gone with
       the old one — a `'rows'` route lists what the server (here, the track
       detail) says, so the count only comes back when this one is opened. */
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Other chat' }));
    await screen.findByRole('button', { name: 'Conversation Other chat, 3 turns' });
    client.setQueryData(queryKeys.trackDetail(TRACK.id), {
      track: TRACK, can_resume: false, cards: [CARD], overlays: [],
    });
    /* Swapped back, and reopened: the original row is listed again and counts
       again. Both directions on one panel instance, which is what a route that
       forked on its planner card could not do. */
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Planner chat' }));
    await screen.findByRole('button', { name: 'Conversation Planner chat, 3 turns' });
  });

  /*
   * `clears an unclaimed open request after a track without a planner card
   * resolves` stood here, and #1341 took its producer away.
   *
   * An "open request" is a card id one route leaves in the registry for another
   * route to redeem, and Today's cross-track list was the only thing that ever
   * left one for a card the arriving track might not have. The one producer
   * left is #1211's planner-open intent, which names a card of the very route it
   * arms on and never names a missing one (`TrackRouteBody` returns early when
   * there is no planner card). So this test had no production driver left, and
   * keeping it would have meant driving the registry by hand to prove a rule
   * about a request nothing makes.
   *
   * The clears themselves are kept as fail-safes and say so at their site; the
   * cross-track card, on its own issue, is what would bring the producer back.
   */

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
    /* The history is fetched when the row is *opened* now — the track route no
       longer holds a card scope before that — so the transcript lands a round
       trip after the drawer does. */
    await screen.findByText('interleaved reply');
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });
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
    const { requests } = setup((request) => request.path.endsWith('/planner/input') ? pending : undefined);
    await openConversation();
    const field = messageField();
    await typeInto(field, 'hello');
    await sendWithEnter(field);
    await sendWithEnter(field);
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
    reject(new Error('send exploded'));
    expect((await screen.findByRole('alert')).textContent).toContain('Transport request failed');
  });

  it('invalidates history and phase after a successful send', async () => {
    const { requests } = setup();
    await openConversation();
    await waitFor(() => {
      expect(requests.filter((request) => request.path.includes('/harness/items')).length).toBeGreaterThan(0);
      expect(requests.filter((request) => request.path.endsWith('/planner/run')).length).toBeGreaterThan(0);
    });
    const beforeHistory = requests.filter((request) => request.path.includes('/harness/items')).length;
    const beforeRun = requests.filter((request) => request.path.endsWith('/planner/run')).length;
    const field = messageField();
    await typeInto(field, 'hello');
    await sendWithEnter(field);
    await waitFor(() => {
      expect(requests.filter((request) => request.path.includes('/harness/items'))).toHaveLength(beforeHistory + 1);
      expect(requests.filter((request) => request.path.endsWith('/planner/run'))).toHaveLength(beforeRun + 1);
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
      if (request.path.endsWith('/planner/run')) {
        return ok({ card_id: CARD.id, worker_session_id: 'runtime', phase: 'turn_running' });
      }
      return request.path.endsWith('/planner/interrupt') ? pendingInterrupt : undefined;
    });
    await openConversation();
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });
    /* The phase query starts with the drawer, so the turn is only known to be
       running a round trip later — and Escape does nothing until it is. */
    await waitFor(() => expect(requests.some((request) => request.path.endsWith('/planner/run'))).toBe(true));
    await waitFor(() => {
      fireEvent.keyDown(drawer, { key: 'Escape' });
      expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(1);
    });
    fireEvent.keyDown(drawer, { key: 'Escape' });
    expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(1);
    expect(screen.getByRole('complementary', { name: 'Planner chat' })).toBeTruthy();
    resolveInterrupt(ok({ card_id: CARD.id, worker_session_id: 'runtime', stopped: true }));
  });

  /*
   * `cancels reset on Escape without also closing the drawer` stood here. The
   * Escape layering it protected — an inner surface eats the key before the
   * drawer does — is unchanged and still needs a guard; its new subject is the
   * `/` command menu, and that test lives in `area-conversation.test.tsx`
   * beside the rest of the slash-command behaviour, because the track route
   * deliberately has no `/` menu (see `startAnother` in the router).
   */
});
