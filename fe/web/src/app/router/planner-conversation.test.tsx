// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { HarnessPhaseTag } from '../../../../core/api/generated/wire.js';
import { HARNESS_ITEMS_PAGE_LIMIT as TRANSCRIPT_PAGE_LIMIT } from '../../../../core/domain/conversation.ts';
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
/* `model` and `reasoning_effort` are required in the response schema; `null` in
   both is a conversation following the installation default. */
const PLANNER_RUN_IDLE = {
  card_id: CARD.id, worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null,
};

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

/* The kernel's `kernel/track/activity` overlay for `w1`: a wedged planner's verdict
   is still `working` until the next tick. */
const trackActivityOverlay = (cards: readonly { card_id: string; state: 'working' | 'input' | 'failed' }[]) => ({
  id: 'activity-w1', plugin_id: 'kernel', entity_kind: 'track', entity_id: TRACK.id, kind: 'activity',
  payload: { schemaVersion: 1, working: cards.length > 0, attention: 'none', activity_at_ms: null, items: [], cards },
  updated_at: 3,
});

function transcriptQueryKey() {
  return queryKeys.harnessItems(CARD.id);
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

/** The open drawer, as a root for what is and is not inside it. */
function drawerElement(): HTMLElement {
  return screen.getByRole('complementary', { name: 'Planner chat' });
}

/* `combobox`, not `textbox`: the composer carries the `/` command menu, and
   `useTriggerMenu` only emits the combobox role when a trigger is configured. */
function messageField(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

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

/* `fireEvent.change` cannot drive Astryx's `contenteditable` composer (no value
   setter) and there is no `<form>`: text is written into the editable, an `input`
   event feeds React state, and Enter sends. */
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

/** Wait out one `POST /planner/input` plus one macrotask, past the promise chain's `finally` where `sending` is released. */
async function settleOneSend(requests: ApiRequest[], expected = 1) {
  await waitFor(() => {
    expect(requests.filter((request) => request.path.endsWith('/planner/input')))
      .toHaveLength(expected);
  });
  await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 0); }); });
}

/** `HarnessState::can_issue_turn()` is `Idle | TurnCompleted`; everything else queues, except `wedged`, whose queue never drains and is blocked instead. */
type PhaseSendPolicy = 'issued' | 'queued' | 'stalled';
const PHASE_SEND_TABLE: Readonly<Record<HarnessPhaseTag, PhaseSendPolicy>> = Object.freeze({
  idle: 'issued',
  turn_completed: 'issued',
  pending_thread_start: 'queued',
  issuing_turn: 'queued',
  issuing_interrupt: 'queued',
  turn_running: 'queued',
  resumed: 'queued',
  wedged: 'stalled',
} satisfies Record<HarnessPhaseTag, PhaseSendPolicy>);

/* The annotation rejects a missing key and `satisfies` rejects an extra one, so a
   new phase cannot land quietly on either side of this table. */
const PHASE_SENDS = Object.freeze(Object.entries(PHASE_SEND_TABLE));

describe('planner conversation regressions', () => {
  it('does not repeat the run phase above the composer while the planner is working', async () => {
    setupWithTurns((request) => request.path.endsWith('/planner/run')
      ? ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running' })
      : undefined);
    await openConversationWithTurns();
    expect(screen.queryByText(/Turn complete|Still working|Stopping this turn/)).toBeNull();
    expect(screen.getByRole('button', { name: 'Stop' })).toBeTruthy();
  });

  it('sends the chosen model to the server when the picker is used', async () => {
    const { requests } = setupWithTurns((request) => request.method === 'PUT'
        && request.path.endsWith('/planner/model')
      ? ok({
        card_id: CARD.id, model: 'gpt-5', reasoning_effort: null,
        effort_adjusted: false, unknown_model: false,
      })
      : undefined);
    await openConversationWithTurns();
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });
    fireEvent.click(within(drawer).getByRole('button', { name: /^Model:/ }));
    /* Nothing answers `GET /api/models` here, so "Default" is the one choice the menu can offer. */
    fireEvent.click(await screen.findByRole('menuitem', { name: /^Default/ }));

    await waitFor(() => {
      const writes = requests.filter((request) => request.path.endsWith('/planner/model'));
      expect(writes).toHaveLength(1);
      expect(writes[0]?.method).toBe('PUT');
      /* Both keys, always: the server answers 422 for a body missing one. */
      expect(writes[0]?.body).toEqual({ model: null, reasoning_effort: null });
    });
  });

  it('shows why a queued message is not being sent when the reader has to act', async () => {
    setupWithTurns((request) => request.path.endsWith('/planner/run')
      ? ok({
        ...PLANNER_RUN_IDLE,
        blocked_reason: 'This conversation follows the default model, and codex\'s configuration '
          + 'does not name one. Pick a model to start it again.',
      })
      : undefined);
    await openConversationWithTurns();
    expect(await screen.findByText(/Pick a model to start it again/)).toBeTruthy();
  });

  it('says nothing when the conversation is not blocked', async () => {
    setupWithTurns();
    await openConversationWithTurns();
    expect(screen.queryByText(/Pick a model to start it again/)).toBeNull();
  });

  it('offers exactly the close, Send and the model picker, and no other control at all', async () => {
    setupWithTurns();
    await openConversationWithTurns();
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });
    const names = within(drawer)
      .getAllByRole('button', { hidden: true })
      .map((button) => button.getAttribute('aria-label') ?? button.textContent);
    /* No catalog answers `GET /api/models`, so the trigger reads `Model: Default` and no effort control appears. */
    expect([...names].sort()).toEqual([
      'Attach an image', 'Close conversation', 'Model: Default', 'Send',
    ]);
    expect(screen.queryByRole('button', { name: /reset/i })).toBeNull();
  });

  /* The server still serves `POST /planner/reset`; this pins that the front end has no path to it. */
  it('never posts to the planner reset endpoint, however the drawer is driven', async () => {
    const { requests, router } = setupWithTurns();
    await openConversationWithTurns();
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });

    const field = within(drawer).getByRole('combobox', { name: 'Message' });
    await typeInto(field, 'a message');
    await sendWithEnter(field);

    const controls = within(drawer)
      .getAllByRole('button', { hidden: true })
      .filter((button) => button.getAttribute('aria-label') !== 'Close conversation');
    for (const control of controls) fireEvent.click(control);
    fireEvent.keyDown(document, { key: 'Escape' });
    fireEvent.click(within(drawer).getByRole('button', { name: 'Close conversation' }));

    await act(async () => { await router.navigate({ to: '/' }); });
    await act(async () => { await router.navigate({ to: '/track/w1' }); });
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    expect(requests.filter((request) => request.path.endsWith('/planner/reset'))).toHaveLength(0);
    /* The pressing above actually did something, so an inert sweep cannot pass. */
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
  });

  it('offers /new in the track composer, now that a track can hold a second conversation', async () => {
    setupWithTurns();
    await openConversationWithTurns();
    const field = messageField();
    expect(field.getAttribute('aria-haspopup')).toBe('listbox');
    expect(screen.queryByRole('textbox', { name: 'Message' })).toBeNull();
  });

  it('keeps a track route conversation list scoped after visiting another track', async () => {
    const { router } = setup();
    await screen.findByRole('button', { name: 'Conversation Planner chat' });
    await router.navigate({ to: '/track/w2' });
    await screen.findByRole('button', { name: 'Conversation Second chat' });
    expect(screen.queryByRole('button', { name: 'Conversation Planner chat' })).toBeNull();
  });

  it('follows a card swapped out and back, counting whichever row is open', async () => {
    const { client } = setup((request) => request.path.includes('/harness/items')
      ? ok(harnessRows(3)) : undefined);
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Planner chat' }));
    /* Waiting for the count is also how we know the transcript has arrived before the card is swapped. */
    await screen.findByRole('button', { name: 'Conversation Planner chat, 3 turns' });

    client.setQueryData(queryKeys.trackDetail(TRACK.id), {
      track: TRACK, can_resume: false, cards: [CARD_SAME_TRACK], overlays: [],
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Other chat' }));
    await screen.findByRole('button', { name: 'Conversation Other chat, 3 turns' });
    client.setQueryData(queryKeys.trackDetail(TRACK.id), {
      track: TRACK, can_resume: false, cards: [CARD], overlays: [],
    });
    fireEvent.click(await screen.findByRole('button', { name: 'Conversation Planner chat' }));
    await screen.findByRole('button', { name: 'Conversation Planner chat, 3 turns' });
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
    /* The history is fetched when the row is opened, so the transcript lands a round trip after the drawer. */
    await screen.findByText('interleaved reply');
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });
    const actionIndex = drawer.textContent?.indexOf('Ran') ?? -1;
    const replyIndex = drawer.textContent?.indexOf('interleaved reply') ?? -1;
    expect(actionIndex).toBeGreaterThanOrEqual(0);
    expect(replyIndex).toBeGreaterThanOrEqual(0);
    expect(actionIndex).toBeLessThan(replyIndex);
  });

  /* `item/started` and `item/completed` pair on `item_uuid`, which is what keeps the group's identity while its last call finishes. */
  it('folds consecutive actions into one closed group that stays open as a call completes', async () => {
    const base = harnessRows(1)[0];
    const command = (id: number, uuid: string, method: string, item: Record<string, unknown>) => ({
      ...base, id, item_uuid: uuid, item_type: 'commandExecution', method, params: JSON.stringify({ item }),
    });
    let rows = [
      { ...base, id: 1, item_uuid: 'message-1', params: JSON.stringify({ item: { text: 'Let me check.' } }) },
      command(2, 'command-1', 'item/completed', { command: 'npm test', exitCode: 1, aggregatedOutput: 'error: no test specified\n' }),
      command(3, 'command-2', 'item/completed', { command: 'ls', exitCode: 0, durationMs: 12 }),
      command(4, 'command-3', 'item/started', { command: 'cargo build' }),
    ];
    const { client } = setup((request) => request.path.includes('/harness/items') ? ok(rows) : undefined);
    await openConversation();
    const group = await screen.findByRole('group', { name: '3 tool calls' });
    const header = within(group).getByRole('button', { expanded: false });
    expect(header.textContent).toContain('Running');
    expect(header.textContent).toContain('cargo build');
    /* The reply before the run is not folded into it. */
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });
    const text = drawer.textContent ?? '';
    expect(text.indexOf('Let me check.')).toBeGreaterThanOrEqual(0);
    expect(text.indexOf('Let me check.')).toBeLessThan(text.indexOf('Running'));
    expect(screen.queryByText('error: no test specified')).toBeNull();

    fireEvent.click(header);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    /* The next poll completes the running call: same `item_uuid`, so the same
       row and the same group — still open, still three. */
    rows = [...rows, command(5, 'command-3', 'item/completed', { command: 'cargo build', exitCode: 0, durationMs: 4_300 })];
    await act(async () => { await client.invalidateQueries({ queryKey: transcriptQueryKey() }); });
    await screen.findByText('4.3s');
    expect(screen.getByRole('group', { name: '3 tool calls' })).toBe(group);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(within(group).queryByText('Running')).toBeNull();
    fireEvent.click(within(group).getByRole('button', { name: /npm test/ }));
    expect(within(group).getByText('error: no test specified')).toBeTruthy();
  });

  /* The page boundary is made here by the infinite query's cursor (`after_id` = the
     oldest id of the page before), and can fall inside a run of calls. */
  it('extends a group cut by the page boundary when Load earlier brings the rest of its run', async () => {
    const base = harnessRows(1)[0];
    const command = (id: number, uuid: string, method: string, item: Record<string, unknown>) => ({
      ...base, id, item_uuid: uuid, item_type: 'commandExecution', method, params: JSON.stringify({ item }), created_at_ms: id,
    });
    const reply = (id: number, text: string) => ({ ...base, id, params: JSON.stringify({ item: { text } }), created_at_ms: id });
    /* The newest page starts with the run's last two calls; replies fill it to the limit. */
    const firstPage = [
      command(101, 'command-101', 'item/completed', { command: 'npm test', exitCode: 1, aggregatedOutput: 'error: no test specified\n' }),
      command(102, 'command-102', 'item/completed', { command: 'pwd', exitCode: 0, durationMs: 12 }),
      ...Array.from({ length: TRANSCRIPT_PAGE_LIMIT - 2 }, (_, index) => reply(103 + index, `reply ${103 + index}`)),
    ];
    /* The page before it ends with the same run's first two calls. */
    const earlierPage = [
      reply(98, 'Let me check.'),
      command(99, 'command-99', 'item/completed', { command: 'old command 1', exitCode: 0 }),
      command(100, 'command-100', 'item/completed', { command: 'old command 2', exitCode: 0 }),
    ];
    const { requests } = setup((request) => request.path.includes('/harness/items')
      ? ok(request.path.includes('after_id=0&') ? firstPage : earlierPage) : undefined);
    await openConversation();
    const group = await screen.findByRole('group', { name: '2 tool calls' });
    const header = within(group).getByRole('button', { expanded: false });
    fireEvent.click(header);
    fireEvent.click(within(group).getByRole('button', { name: /npm test/ }));
    const detail = within(group).getByText('error: no test specified');

    fireEvent.click(screen.getByRole('button', { name: 'Load earlier' }));
    expect(await screen.findByRole('group', { name: '4 tool calls' })).toBe(group);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(within(group).getByText('error: no test specified')).toBe(detail);
    const order = ['old command 1', 'old command 2', 'npm test', 'pwd'].map((target) => (group.textContent ?? '').indexOf(target));
    expect(order.every((position, index) => position >= 0 && (index === 0 || position > order[index - 1]))).toBe(true);
    const drawer = screen.getByRole('complementary', { name: 'Planner chat' });
    expect((drawer.textContent ?? '').indexOf('Let me check.')).toBeLessThan((drawer.textContent ?? '').indexOf('old command 1'));
    expect(requests.filter((request) => request.path.includes('/harness/items'))
      .map((request) => new URL(request.path, 'http://localhost').searchParams.get('after_id'))).toEqual(['0', '101']);
  });

  /* A refetch re-reads the newest page from the top and can leave the open run with
     one call in the window (a line, not a group); the vendor element does not survive that. */
  it('keeps an open group and its opened failure detail through a refetch that leaves it one call', async () => {
    const base = harnessRows(1)[0];
    const command = (id: number, uuid: string, item: Record<string, unknown>) => ({
      ...base, id, item_uuid: uuid, item_type: 'commandExecution', method: 'item/completed',
      params: JSON.stringify({ item }), created_at_ms: id,
    });
    const reply = (id: number) => ({
      ...base, id, params: JSON.stringify({ item: { text: `reply ${id}` } }), created_at_ms: id,
    });
    const failed = command(102, 'command-102', {
      command: 'npm test', exitCode: 1, aggregatedOutput: 'failure detail\n',
    });
    const initial = [
      command(101, 'command-101', { command: 'pwd', exitCode: 0 }), failed,
      ...Array.from({ length: TRANSCRIPT_PAGE_LIMIT - 2 }, (_, i) => reply(103 + i)),
    ];
    const shifted = [
      failed,
      ...Array.from({ length: TRANSCRIPT_PAGE_LIMIT - 1 }, (_, i) => reply(103 + i)),
    ];
    let refetched = false;
    const { client } = setup((request) => request.path.includes('/harness/items')
      ? ok(request.path.includes('after_id=0&') ? (refetched ? shifted : initial) : [initial[0]])
      : undefined);
    await openConversation();
    const group = await screen.findByRole('group', { name: '2 tool calls' });
    const header = within(group).getByRole('button', { expanded: false });
    fireEvent.click(header);
    fireEvent.click(within(group).getByRole('button', { name: /npm test/ }));
    expect(within(group).getByText('failure detail')).toBeTruthy();

    refetched = true;
    await act(async () => { await client.invalidateQueries({ queryKey: transcriptQueryKey() }); });
    await waitFor(() => expect(screen.queryByRole('group', { name: '2 tool calls' })).toBeNull());
    expect(screen.getByText('failure detail')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Load earlier' }));
    const restored = await screen.findByRole('group', { name: '2 tool calls' });
    expect(within(restored).getByRole('button', { expanded: true }).getAttribute('aria-expanded')).toBe('true');
    expect(within(restored).getByText('failure detail')).toBeTruthy();
    expect((restored.textContent ?? '').indexOf('pwd')).toBeLessThan((restored.textContent ?? '').indexOf('npm test'));
    fireEvent.click(within(restored).getByRole('button', { name: /npm test/ }));
    expect(within(restored).queryByText('failure detail')).toBeNull();
  });

  /* One row wider: the vendor's element stays mounted while the row the reader had open is unmounted from inside it. */
  it('restores an opened failed row after a multi-call refetch window shift', async () => {
    const base = harnessRows(1)[0];
    const command = (id: number, item: Record<string, unknown>) => ({
      ...base, id, item_uuid: `command-${id}`, item_type: 'commandExecution', method: 'item/completed',
      params: JSON.stringify({ item }), created_at_ms: id,
    });
    const reply = (id: number) => ({
      ...base, id, params: JSON.stringify({ item: { text: `reply ${id}` } }), created_at_ms: id,
    });
    const failed = command(101, { command: 'npm test', exitCode: 1, aggregatedOutput: 'failure evidence\n' });
    const retained = [command(102, { command: 'pwd', exitCode: 0 }), command(103, { command: 'ls', exitCode: 0 })];
    const initial = [failed, ...retained, ...Array.from({ length: TRANSCRIPT_PAGE_LIMIT - 3 }, (_, i) => reply(104 + i))];
    const shifted = [...retained, ...Array.from({ length: TRANSCRIPT_PAGE_LIMIT - 2 }, (_, i) => reply(104 + i))];
    let refetched = false;
    const { client } = setup((request) => request.path.includes('/harness/items')
      ? ok(request.path.includes('after_id=0&') ? (refetched ? shifted : initial) : [failed])
      : undefined);
    await openConversation();
    const group = await screen.findByRole('group', { name: '3 tool calls' });
    const header = within(group).getByRole('button', { expanded: false });
    fireEvent.click(header);
    fireEvent.click(within(group).getByRole('button', { name: /npm test/ }));
    expect(within(group).getByText('failure evidence')).toBeTruthy();

    refetched = true;
    await act(async () => { await client.invalidateQueries({ queryKey: transcriptQueryKey() }); });
    expect(await screen.findByRole('group', { name: '2 tool calls' })).toBe(group);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(within(group).queryByText('failure evidence')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Load earlier' }));
    expect(await screen.findByRole('group', { name: '3 tool calls' })).toBe(group);
    expect(header.getAttribute('aria-expanded')).toBe('true');
    expect(within(group).getByText('failure evidence')).toBeTruthy();
  });

  /* The shift that takes the whole run: it is remembered by its calls' ids for as long as the conversation is open. */
  it('restores the same group after it leaves the latest page whole', async () => {
    const base = harnessRows(1)[0];
    const command = (id: number, item: Record<string, unknown>) => ({
      ...base, id, item_uuid: `command-${id}`, item_type: 'commandExecution', method: 'item/completed',
      params: JSON.stringify({ item }), created_at_ms: id,
    });
    const reply = (id: number) => ({
      ...base, id, params: JSON.stringify({ item: { text: `reply ${id}` } }), created_at_ms: id,
    });
    const calls = [
      command(101, { command: 'npm test', exitCode: 1, aggregatedOutput: 'whole-run failure evidence\n' }),
      command(102, { command: 'pwd', exitCode: 0 }),
    ];
    const initial = [...calls, ...Array.from({ length: TRANSCRIPT_PAGE_LIMIT - 2 }, (_, i) => reply(103 + i))];
    const shifted = Array.from({ length: TRANSCRIPT_PAGE_LIMIT }, (_, i) => reply(103 + i));
    let refetched = false;
    const { client } = setup((request) => request.path.includes('/harness/items')
      ? ok(request.path.includes('after_id=0&') ? (refetched ? shifted : initial) : calls)
      : undefined);
    await openConversation();
    const group = await screen.findByRole('group', { name: '2 tool calls' });
    fireEvent.click(within(group).getByRole('button', { expanded: false }));
    fireEvent.click(within(group).getByRole('button', { name: /npm test/ }));
    expect(within(group).getByText('whole-run failure evidence')).toBeTruthy();

    refetched = true;
    await act(async () => { await client.invalidateQueries({ queryKey: transcriptQueryKey() }); });
    await waitFor(() => expect(screen.queryByRole('group', { name: '2 tool calls' })).toBeNull());
    fireEvent.click(screen.getByRole('button', { name: 'Load earlier' }));
    const restored = await screen.findByRole('group', { name: '2 tool calls' });
    expect(restored.querySelector('[aria-expanded]')?.getAttribute('aria-expanded')).toBe('true');
    expect(within(restored).getByText('whole-run failure evidence')).toBeTruthy();
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
      ? ok(harnessRows(TRANSCRIPT_PAGE_LIMIT)) : undefined);
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

  /* `send_planner_input` accepts at any phase and queues the text behind the running
     turn; `data-nc-queued` is present only on the send that was actually queued. */
  it('posts a message sent while a turn is running, and marks it queued', async () => {
    const { requests } = setup((request) => request.path.endsWith('/planner/run')
      ? ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running' })
      : undefined);
    await openConversation();
    /* The phase arrives a round trip after the drawer, and "running" is the
       precondition — without it this would be an ordinary idle send. */
    await screen.findByRole('button', { name: 'Stop' });

    const field = messageField();
    await typeInto(field, 'queued words');
    await sendWithEnter(field);

    await waitFor(() => {
      expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);
    });
    expect(await screen.findByText('queued words')).toBeTruthy();
    const queued = document.querySelector('[data-nc-queued]');
    expect(queued?.textContent).toBe('queued words');
    expect(document.querySelector('[data-nc-queued-note]')?.textContent)
      .toContain('sends when this turn ends');
  });

  /* Once the POST answers with an entry id and `GET /planner/run` lists it, the queue
     region owns the message and the transcript echo steps aside. */
  it('draws a queued message once, in the queue region, after its entry id is listed', async () => {
    const entry = { entry_id: 'entry-9', text: 'queued once', rev: 0, queued_at_ms: 5 };
    let listed = false;
    setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({
          ...PLANNER_RUN_IDLE, phase: 'turn_running',
          pending: listed ? [entry] : [], pending_overflow: 0,
        });
      }
      if (request.method === 'POST' && request.path.endsWith('/planner/input')) {
        listed = true;
        return ok({ card_id: CARD.id, worker_session_id: 'runtime', entry_id: entry.entry_id });
      }
      return undefined;
    });
    await openConversation();
    await screen.findByRole('button', { name: 'Stop' });

    const field = messageField();
    await typeInto(field, 'queued once');
    await sendWithEnter(field);

    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-queue]')).not.toBeNull();
    });
    await waitFor(() => {
      expect(screen.getAllByText('queued once')).toHaveLength(1);
    });
    expect(document.querySelector('[data-nc-pending-entry="entry-9"]')?.textContent)
      .toContain('queued once');
    expect(document.querySelector('[data-nc-queued]')).toBeNull();
  });

  /* The drain writes the projection row before `turn/start` goes out; the queue region
     drops the entry only once `planner-run` is refetched after `turn/start` answers. */
  it('draws a drained message once while the queue region still lists its entry', async () => {
    const entry = { entry_id: 'entry-9', text: 'sent once', rev: 0, queued_at_ms: 5 };
    const projection = {
      id: 7, worker_session_id: 'runtime', card_id: CARD.id, track_id: TRACK.id, thread_id: 'thread',
      turn_id: null, item_uuid: entry.entry_id, item_type: 'userMessage', method: 'item/completed',
      params: JSON.stringify({
        item: { id: entry.entry_id, clientId: entry.entry_id, type: 'userMessage', content: [{ type: 'text', text: 'sent once' }] },
        _projection: true,
      }),
      input_segments: [{ presentation: 'user', text: 'sent once', attachments: [] }],
      created_at_ms: 7,
    };
    let listed = true;
    const { client } = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({
          ...PLANNER_RUN_IDLE, phase: 'issuing_turn',
          pending: listed ? [entry] : [], pending_overflow: 0,
        });
      }
      if (request.path.includes('/harness/items')) return ok([projection]);
      return undefined;
    });
    await openConversation();

    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')?.textContent).toContain('sent once');
    });
    expect(screen.getAllByText('sent once')).toHaveLength(1);
    expect(document.querySelector('[data-nc-turn="you"]')).toBeNull();

    // The phase change after `turn/start` refetches `planner-run`.
    listed = false;
    await act(async () => {
      await client.invalidateQueries({ queryKey: queryKeys.plannerRun(CARD.id) });
    });
    await waitFor(() => {
      expect(document.querySelector('[data-nc-turn="you"]')?.textContent).toContain('sent once');
    });
    expect(screen.getAllByText('sent once')).toHaveLength(1);
    expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).toBeNull();
  });

  /* The revision is the entry's, read from the page the reader was shown, not the client's guess. */
  it('sends the listed revision as if_entry_rev when removing a queued message', async () => {
    const entry = { entry_id: 'entry-9', text: 'first words', rev: 3, queued_at_ms: 5 };
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running', pending: [entry], pending_overflow: 0 });
      }
      if (request.method === 'DELETE' && request.path.includes('/planner/input/')) {
        return ok({ card_id: CARD.id, entry_id: entry.entry_id, rev: 4, text: null });
      }
      return undefined;
    });
    await openConversation();
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
    });

    fireEvent.click(screen.getByRole('button', { name: 'Delete this message' }));

    await waitFor(() => {
      expect(requests.some((request) => request.method === 'DELETE')).toBe(true);
    });
    const removal = requests.find((request) => request.method === 'DELETE');
    expect(removal?.path).toBe(`/api/cards/${CARD.id}/planner/input/entry-9`);
    expect(removal?.body).toEqual({ if_entry_rev: 3 });
  });

  /* `issuing_turn` is `working` and yet a steer there answers 409, because the turn
     does not exist yet: the gate is on `turn_running`, not `working`. */
  it('offers "Say it now" while a turn is running, and posts the listed revision to the steer route', async () => {
    const entry = { entry_id: 'entry-9', text: 'now please', rev: 3, queued_at_ms: 5 };
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running', pending: [entry], pending_overflow: 0 });
      }
      if (request.method === 'POST' && request.path.endsWith('/steer')) {
        return ok({ card_id: CARD.id, entry_id: entry.entry_id, steered: true, turn_id: 'turn-1' });
      }
      return undefined;
    });
    await openConversation();
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
    });

    fireEvent.click(screen.getByRole('button', { name: 'Say it now' }));

    await waitFor(() => {
      expect(requests.some((request) => request.path.endsWith('/steer'))).toBe(true);
    });
    const steer = requests.find((request) => request.path.endsWith('/steer'));
    expect(steer?.method).toBe('POST');
    expect(steer?.path).toBe(`/api/cards/${CARD.id}/planner/input/entry-9/steer`);
    expect(steer?.body).toEqual({ if_entry_rev: 3 });
    /* A 200 forgets the bubble at once, as a delete does. */
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).toBeNull();
    });
  });

  /* After a steer's 200 the turn can end before codex records the message; the kernel
     puts the entry back under the same id, one rev up. A refetch still listing rev 3
     is a stale page and keeps the bubble hidden; rev 4 is the kernel's word it came back. */
  it('shows a steered message again, with its controls, once the server lists it at a higher rev', async () => {
    let served = { entry_id: 'entry-9', text: 'came back', rev: 3, queued_at_ms: 5 };
    let runReads = 0;
    const { client, requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        runReads += 1;
        return ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running', pending: [served], pending_overflow: 0 });
      }
      if (request.method === 'POST' && request.path.endsWith('/steer')) {
        return ok({ card_id: CARD.id, worker_session_id: 'runtime', entry_id: served.entry_id, steered: true, turn_id: 'turn-1' });
      }
      return undefined;
    });
    await openConversation();
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
    });
    const readsBeforeSteer = runReads;

    fireEvent.click(screen.getByRole('button', { name: 'Say it now' }));
    await waitFor(() => {
      expect(requests.some((request) => request.path.endsWith('/steer'))).toBe(true);
    });
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).toBeNull();
    });
    /* The refetch the 200 triggers still lists the rev this client steered against: a
           stale page, so the bubble stays hidden. */
    await waitFor(() => { expect(runReads).toBeGreaterThan(readsBeforeSteer); });
    await waitFor(() => { expect(client.isFetching({ queryKey: queryKeys.plannerRun(CARD.id) })).toBe(0); });
    expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).toBeNull();

    /* The kernel put it back, one rev up, and `restored` refetched the page. */
    served = { ...served, rev: 4 };
    await act(async () => {
      await client.invalidateQueries({ queryKey: queryKeys.plannerRun(CARD.id) });
    });
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')?.textContent).toContain('came back');
    });
    expect(screen.getByRole('button', { name: 'Say it now' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Delete this message' })).toBeTruthy();
    /* And a steer from here writes against the rev the page lists now. */
    fireEvent.click(screen.getByRole('button', { name: 'Say it now' }));
    await waitFor(() => {
      expect(requests.filter((request) => request.path.endsWith('/steer'))).toHaveLength(2);
    });
    expect(requests.filter((request) => request.path.endsWith('/steer'))[1]?.body).toEqual({ if_entry_rev: 4 });
  });

  it('does not offer "Say it now" while the turn is still being issued', async () => {
    const entry = { entry_id: 'entry-9', text: 'not yet', rev: 0, queued_at_ms: 5 };
    setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ ...PLANNER_RUN_IDLE, phase: 'issuing_turn', pending: [entry], pending_overflow: 0 });
      }
      return undefined;
    });
    await openConversation();
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
    });
    expect(screen.getByRole('button', { name: 'Delete this message' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Say it now' })).toBeNull();
  });

  it('says the message stays queued when the steer finds no running turn', async () => {
    const entry = { entry_id: 'entry-9', text: 'too late', rev: 0, queued_at_ms: 5 };
    setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running', pending: [entry], pending_overflow: 0 });
      }
      if (request.method === 'POST' && request.path.endsWith('/steer')) {
        return {
          status: 409, statusText: 'Conflict',
          body: {
            error: 'no turn is running right now', code: 'planner_steer_no_running_turn',
            entry_id: entry.entry_id, phase: 'turn_completed',
          },
        };
      }
      return undefined;
    });
    await openConversation();
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
    });

    fireEvent.click(screen.getByRole('button', { name: 'Say it now' }));

    expect(await screen.findByText(/stays queued and will go with the next turn/)).toBeTruthy();
    expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
  });

  /* The steer's other 409: codex never answered, so the message may have reached the
       turn AND be queued again. */
  it('says the outcome is not known when the steer times out on the kernel side', async () => {
    const entry = { entry_id: 'entry-9', text: 'did it land', rev: 0, queued_at_ms: 5 };
    setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running', pending: [entry], pending_overflow: 0 });
      }
      if (request.method === 'POST' && request.path.endsWith('/steer')) {
        return {
          status: 409, statusText: 'Conflict',
          body: {
            error: 'codex did not answer in time', code: 'planner_steer_unknown_outcome',
            entry_id: entry.entry_id, phase: 'turn_running',
          },
        };
      }
      return undefined;
    });
    await openConversation();
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
    });

    fireEvent.click(screen.getByRole('button', { name: 'Say it now' }));

    const notice = await screen.findByText(/not known whether this message reached/);
    expect(notice.textContent).toMatch(/stays queued and will go with the next turn/);
    expect(notice.textContent).not.toMatch(/nothing happened/);
    expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
  });

  /* Deleting un-hides the transcript echo, and nothing else can retire it: the message
     never reaches the model, so no row will arrive to reconcile it. */
  it('does not put a deleted queued message back into the transcript', async () => {
    const entry = { entry_id: 'entry-9', text: 'take this back', rev: 0, queued_at_ms: 5 };
    let deleted = false;
    let listed = false;
    setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({
          ...PLANNER_RUN_IDLE, phase: 'turn_running',
          pending: listed && !deleted ? [entry] : [], pending_overflow: 0,
        });
      }
      if (request.method === 'POST' && request.path.endsWith('/planner/input')) {
        listed = true;
        return ok({ card_id: CARD.id, worker_session_id: 'runtime', entry_id: entry.entry_id });
      }
      if (request.method === 'DELETE' && request.path.includes('/planner/input/')) {
        deleted = true;
        return ok({ card_id: CARD.id, entry_id: entry.entry_id, rev: 1, text: null });
      }
      return undefined;
    });
    await openConversation();
    await screen.findByRole('button', { name: 'Stop' });
    const field = messageField();
    await typeInto(field, 'take this back');
    await sendWithEnter(field);
    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).not.toBeNull();
    });

    fireEvent.click(screen.getByRole('button', { name: 'Delete this message' }));

    await waitFor(() => {
      expect(document.querySelector('[data-nc-pending-entry="entry-9"]')).toBeNull();
    });
    await waitFor(() => {
      expect(screen.queryAllByText('take this back')).toHaveLength(0);
    });
  });

  it('does not mark a message sent from an idle conversation as queued', async () => {
    setup();
    await openConversation();
    const field = messageField();
    await typeInto(field, 'idle words');
    await sendWithEnter(field);
    expect(await screen.findByText('idle words')).toBeTruthy();
    expect(document.querySelector('[data-nc-queued]')).toBeNull();
    expect(document.querySelector('[data-nc-queued-note]')).toBeNull();
  });

  /* `sendBlocked` is about the request in flight, not the agent. */
  it('still prevents a second send while the first is pending, with a turn running', async () => {
    let settle!: (response: ApiTransportResponse) => void;
    const pending = new Promise<ApiTransportResponse>((resolve) => { settle = resolve; });
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running' });
      }
      return request.path.endsWith('/planner/input') ? pending : undefined;
    });
    await openConversation();
    await screen.findByRole('button', { name: 'Stop' });
    const field = messageField();
    await typeInto(field, 'first');
    await sendWithEnter(field);
    await typeInto(field, 'second');
    await sendWithEnter(field);
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toHaveLength(1);

    /* The release is followed through: a body the response schema rejects would take
         the failure path and leave the count above green. */
    settle(ok({ card_id: CARD.id, worker_session_id: 'runtime' }));
    await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 0); }); });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(messageField().getAttribute('contenteditable')).toBe('true');
    await typeInto(messageField(), 'second, after the release');
    await sendWithEnter(messageField());
    await settleOneSend(requests, 2);
    expect(screen.queryByRole('alert')).toBeNull();
  });

  /* A queued echo can never be counted down: the pending queue writes no transcript
     row until the turn ends. Requests, not DOM: only the transport says a second message left. */
  it('keeps taking messages after one has been queued behind a running turn', async () => {
    const { requests } = setup((request) => request.path.endsWith('/planner/run')
      ? ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running' })
      : undefined);
    await openConversation();
    await screen.findByRole('button', { name: 'Stop' });
    const input = () => requests.filter((request) => request.path.endsWith('/planner/input'));

    const field = messageField();
    await typeInto(field, 'first queued');
    await sendWithEnter(field);
    await waitFor(() => { expect(input()).toHaveLength(1); });

    /* Asserted through the send: `disabled` is one of several ways the box could be dead. */
    await typeInto(field, 'second queued');
    await sendWithEnter(field);
    await waitFor(() => { expect(input()).toHaveLength(2); });
    expect(input().map((request) => (request.body as { text: string }).text))
      .toEqual(['first queued', 'second queued']);
  });

  /* An echo minted from idle closes the composer until the server hands it back. Read
     off `contenteditable` (Astryx's rendering of `isDisabled`), not a refused Enter:
     a settled send has emptied the draft, so a second Enter proves nothing. */
  it('still closes the composer after an idle send until the server hands it back', async () => {
    const { requests } = setup();
    await openConversation();
    await typeInto(messageField(), 'idle send');
    await sendWithEnter(messageField());
    await settleOneSend(requests);
    expect(messageField().getAttribute('contenteditable')).toBe('false');
  });

  it('leaves the composer open after a queued send', async () => {
    const { requests } = setup((request) => request.path.endsWith('/planner/run')
      ? ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running' })
      : undefined);
    await openConversation();
    await screen.findByRole('button', { name: 'Stop' });
    await typeInto(messageField(), 'queued send');
    await sendWithEnter(messageField());
    await settleOneSend(requests);
    expect(messageField().getAttribute('contenteditable')).toBe('true');
  });

  it.each(PHASE_SENDS)(
    'phase %s applies %s policy to markers, composer state and subsequent sends',
    async (phase, policy) => {
      const { client, requests } = setup((request) => request.path.endsWith('/planner/run')
        ? ok({ card_id: CARD.id, worker_session_id: 'runtime', phase, model: null, reasoning_effort: null, blocked_reason: null })
        /* The wedged case also carries the kernel's stale `working` verdict: the drawer's
                   own wedge must outrank it. */
        : policy === 'stalled' && request.path === '/api/tracks/w1'
          ? ok({ track: TRACK, can_resume: false, cards: [CARD],
              overlays: [trackActivityOverlay([{ card_id: CARD.id, state: 'working' }])] })
          : undefined);
      await openConversation();
      /* Seeding the cache removes the window in which `phase` is `null` and every row
             would look queued for the wrong reason. */
      await act(async () => {
        client.setQueryData(queryKeys.plannerRun(CARD.id),
          { card_id: CARD.id, worker_session_id: 'runtime', phase, model: null, reasoning_effort: null, blocked_reason: null });
        await Promise.resolve();
      });
      const input = () => requests.filter((request) => request.path.endsWith('/planner/input'));
      if (policy === 'stalled') {
        expect(messageField().getAttribute('contenteditable')).toBe('false');
        expect((await screen.findByRole('alert')).textContent).toContain('This conversation is stuck');
        expect(screen.getByRole('button', { name: 'Start a new conversation' })).toBeTruthy();
        /* The wedged row is `failed`, not a request for input: mapped to `attention` it
                   would read as "waiting for you". */
        const row = screen.getByRole('button', { name: /^Conversation Planner chat(?:,|$)/ });
        expect(row.closest('li')?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity')).toBe('failed');
        expect(document.getElementById(row.getAttribute('aria-describedby') ?? '')?.textContent).toBe('Needs attention');
        /* The overlay says `working`, the local wedge says stuck, and stuck wins in both places. */
        expect(row.getAttribute('aria-label')).not.toContain(', working');
        expect(drawerElement().querySelector('[data-nc-activity="working"]')).toBeNull();
        expect(within(drawerElement()).queryByText('Working')).toBeNull();
        await sendWithEnter(messageField());
        expect(input()).toHaveLength(0);
        expect(document.querySelector('[data-nc-queued]')).toBeNull();
        return;
      }
      const queues = policy === 'queued';
      await typeInto(messageField(), 'first');
      await sendWithEnter(messageField());
      await settleOneSend(requests);

      expect(document.querySelector('[data-nc-queued]') !== null).toBe(queues);
      expect(messageField().getAttribute('contenteditable')).toBe(queues ? 'true' : 'false');

      await typeInto(messageField(), 'second');
      await sendWithEnter(messageField());
      await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 0); }); });
      expect(input()).toHaveLength(queues ? 2 : 1);
    },
  );

  it('keeps Stop working, and offers nothing else beside it', async () => {
    const { requests } = setup((request) => request.path.endsWith('/planner/run')
      ? ok({ ...PLANNER_RUN_IDLE, phase: 'turn_running' })
      : undefined);
    await openConversation();
    const stop = await screen.findByRole('button', { name: 'Stop' });
    expect(screen.queryByRole('button', { name: 'Queue message' })).toBeNull();
    fireEvent.click(stop);
    await waitFor(() => {
      expect(requests.filter((request) => request.path.endsWith('/planner/interrupt'))).toHaveLength(1);
    });
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

  it('uses Escape to interrupt a working turn without closing the drawer', async () => {
    let resolveInterrupt!: (response: ApiTransportResponse) => void;
    const pendingInterrupt = new Promise<ApiTransportResponse>((resolve) => { resolveInterrupt = resolve; });
    const { requests } = setup((request) => {
      if (request.path.endsWith('/planner/run')) {
        return ok({
          card_id: CARD.id, worker_session_id: 'runtime', phase: 'turn_running',
          model: null, reasoning_effort: null, blocked_reason: null,
        });
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

});
