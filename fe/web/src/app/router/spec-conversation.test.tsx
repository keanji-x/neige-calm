// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { HARNESS_ITEMS_PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { queryKeys } from '../providers/queries.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const COVE = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = { id: 'w1', cove_id: 'c1', title: 'Test wave', sort: 1, lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
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
      if (request.path === '/api/coves') return ok([COVE]);
      if (request.path === '/api/coves/c1/waves') return ok([WAVE, WAVE_B]);
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

describe('spec conversation regressions', () => {
  /*
   * Three tests used to stand here, all of them about *when* the reset control
   * appeared: it had to be labelled rather than a glyph, it had to be absent
   * over a zero-turn conversation, and it had to arrive with the first turn.
   * The control is gone (#1139), so none of the three has a subject.
   *
   * What replaces them is one assertion with more force than any of them had,
   * and it is stated as a universal rather than by name: **the drawer offers no
   * destructive control at all**. Naming `Reset conversation` would pass the
   * day someone reintroduces the same action under another label, which is
   * exactly the regression worth fencing; `[data-nc-action='destructive']` is
   * the single vocabulary every red control in this app is drawn from
   * (base.css §4.3), so sweeping for it catches the class and not the string.
   *
   * It is run over the *non-empty* drawer on purpose. The removed control was
   * conditional on there being a transcript, so an empty drawer is precisely
   * the state that never had one — proving nothing. The close must still be
   * there, which is what keeps this from passing on a drawer that failed to
   * render.
   */
  it('offers no destructive control anywhere in an open conversation', async () => {
    setupWithTurns();
    await openConversationWithTurns();
    const drawer = screen.getByRole('complementary', { name: 'Spec chat' });
    expect(drawer.querySelectorAll('[data-nc-action="destructive"]')).toHaveLength(0);
    expect(within(drawer).getByRole('button', { name: 'Close conversation' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /reset/i })).toBeNull();
  });

  /* And the browser never calls the endpoint, on any route, however the drawer
     is driven. The server still serves `POST /spec/reset`; this pins that the
     front end has no path to it — a UI-only removal that left a live caller
     behind would be invisible to the sweep above. */
  it('never posts to the spec reset endpoint', async () => {
    const { requests } = setupWithTurns();
    await openConversationWithTurns();
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    await screen.findByRole('button', { name: /Conversation Spec chat, on Test wave/ });
    expect(requests.filter((request) => request.path.endsWith('/spec/reset'))).toHaveLength(0);
  });

  /*
   * The wave route gets the `+` and deliberately does *not* get `/new` — see
   * `startAnother` in the router: this route has exactly one spec card, so
   * `start()` here reopens the row already open rather than creating anything,
   * and a command named `New conversation` that does nothing is worse than no
   * command. The observable consequence is in the accessibility tree, which is
   * the honest place to assert it: with no trigger configured the field stays
   * a plain `textbox` instead of becoming a combobox that can never expand.
   */
  it('leaves the wave composer a plain textbox, with no / command menu', async () => {
    setupWithTurns();
    await openConversationWithTurns();
    const field = screen.getByRole('textbox', { name: 'Message' });
    expect(field.getAttribute('role')).toBe('textbox');
    expect(field.hasAttribute('aria-haspopup')).toBe(false);
    expect(screen.queryByRole('combobox', { name: 'Message' })).toBeNull();
  });

  it('keeps a wave route conversation list scoped after visiting another wave', async () => {
    const { router } = setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    await router.navigate({ to: '/wave/w2' });
    await screen.findByRole('button', { name: 'Conversation Second chat, 0 turns' });
    expect(screen.queryByRole('button', { name: 'Conversation Spec chat, 0 turns' })).toBeNull();
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

  it('navigates from a Today conversation to its wave before opening it', async () => {
    const { requests } = setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    fireEvent.click(await screen.findByRole('button', {
      name: 'Conversation Spec chat, on Test wave, 0 turns',
    }));
    await screen.findByRole('complementary', { name: 'Spec chat' });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w1`);
    expect(requests.some(({ path }) => path.includes('/api/cards//'))).toBe(false);
  });

  /*
   * A cove lists its *own* conversations, from the server (#1098) — never the
   * spec conversations of the waves inside it. Those hang off a wave and are
   * read on that wave's page; listing them here would put rows in a panel whose
   * drawer this route deliberately opens in place, on a card it has no scope
   * for. Today is still where a remembered wave conversation shows up.
   */
  it('does not list a wave spec conversation on that wave\'s cove', async () => {
    setup();
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    fireEvent.click(screen.getByRole('button', { name: 'Work' }));
    await screen.findByText('No conversations yet.');
    expect(screen.queryByRole('button', { name: /Conversation Spec chat/ })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'neige · calm' }));
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave, 0 turns' });
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
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Spec chat, 3 turns' }));

    client.setQueryData(queryKeys.harnessItems(CARD_SAME_WAVE.id), {
      pages: [harnessRows(3)], pageParams: [0],
    });
    client.setQueryData(queryKeys.waveDetail(WAVE.id), { wave: WAVE, cards: [CARD_SAME_WAVE], overlays: [] });
    await screen.findByRole('button', { name: 'Conversation Other chat, 3 turns' });
    client.setQueryData(queryKeys.waveDetail(WAVE.id), { wave: WAVE, cards: [CARD], overlays: [] });
    await screen.findByRole('button', { name: 'Conversation Spec chat, 3 turns' });
    await router.navigate({ to: '/' });
    await screen.findByRole('button', { name: 'Conversation Spec chat, on Test wave, 3 turns' });
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
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
    await router.navigate({ to: '/' });
    omitTargetCard = true;
    client.removeQueries({ queryKey: queryKeys.waveDetail(WAVE.id) });
    fireEvent.click(await screen.findByRole('button', {
      name: 'Conversation Spec chat, on Test wave, 0 turns',
    }));
    await screen.findByText('No cards yet.');
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();

    await router.navigate({ to: '/wave/w2' });
    await screen.findByRole('button', { name: 'Conversation Spec chat, 0 turns' });
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
    expect(screen.getByText('Thought')).toBeTruthy();
    const field = screen.getByRole('textbox', { name: 'Message' });
    fireEvent.change(field, { target: { value: 'next message' } });
    fireEvent.submit(field.closest('form')!);
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
    const field = screen.getByRole('textbox', { name: 'Message' });
    fireEvent.change(field, { target: { value: 'hello' } });
    fireEvent.submit(field.closest('form')!);
    fireEvent.submit(field.closest('form')!);
    expect(requests.filter((request) => request.path.endsWith('/spec/input'))).toHaveLength(1);
    reject(new Error('send exploded'));
    expect((await screen.findByRole('alert')).textContent).toContain('Transport request failed');
  });

  it('invalidates history and phase after a successful send', async () => {
    const { requests } = setup();
    await openConversation();
    const beforeHistory = requests.filter((request) => request.path.includes('/harness/items')).length;
    const beforeRun = requests.filter((request) => request.path.endsWith('/spec/run')).length;
    const field = screen.getByRole('textbox', { name: 'Message' });
    fireEvent.change(field, { target: { value: 'hello' } });
    fireEvent.submit(field.closest('form')!);
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
    fireEvent.keyDown(drawer, { key: 'Escape' });
    fireEvent.keyDown(drawer, { key: 'Escape' });
    expect(requests.filter((request) => request.path.endsWith('/spec/interrupt'))).toHaveLength(1);
    expect(screen.getByRole('complementary', { name: 'Spec chat' })).toBeTruthy();
    resolveInterrupt(ok({ card_id: CARD.id, runtime_id: 'runtime', stopped: true }));
  });

  /*
   * `cancels reset on Escape without also closing the drawer` stood here. The
   * Escape layering it protected — an inner surface eats the key before the
   * drawer does — is unchanged and still needs a guard; its new subject is the
   * `/` command menu, and that test lives in `cove-conversation.test.tsx`
   * beside the rest of the slash-command behaviour, because the wave route
   * deliberately has no `/` menu (see `startAnother` in the router).
   */
});
