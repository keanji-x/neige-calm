// @vitest-environment jsdom
// The Planner's reply as it streams (#1923 S2 P4), through the production router and a fake transport.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { HarnessLiveReplies, HarnessPhaseTag } from '../../../../core/api/generated/wire.ts';
import type { PlannerRunningTurn, buildTranscript } from '../../../../core/domain/conversation.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { queryKeys } from '../providers/queries.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = { id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
const CARD = { id: 'card-1', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1, payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
/* A second conversation on the same track: a track assistant, listed by the track's conversations. */
const OTHER = { ...CARD, id: 'conv-2', title: 'Other chat', payload: { harness_profile: 'assistant' }, sort: 2 };
const OTHER_ROW = { id: OTHER.id, trackId: TRACK.id, title: OTHER.title, kind: 'track-assistant', state: 'running', updatedAt: 5, lastTurnCompletedAt: null };
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

/** One card's server side: what each of the three reads answers right now. */
type Row = Parameters<typeof buildTranscript>[0][number];
/** With `earlier` rows, the newest page is full, so Load earlier has a page to read; with `itemsFail`, every transcript read fails. */
type CardServer = { runningTurn: PlannerRunningTurn | null; phase: HarnessPhaseTag; rows: Row[]; live: HarnessLiveReplies; earlier?: Row[]; itemsFail?: boolean;
  /** What a send that replaces a turn (#1923 Edit, #2043) does to this card and answers; with one, a send is accepted. */
  replace?: () => ApiTransportResponse };

function row(id: number, method: string, itemType: string | null, params: unknown, extra: Partial<Row> = {}): Row {
  return {
    id, worker_session_id: 'runtime', card_id: CARD.id, track_id: TRACK.id, thread_id: 'thread', turn_id: 'T1',
    turn_error_text: null, item_uuid: null, item_type: itemType, method, params: JSON.stringify(params), created_at_ms: id,
    ...extra,
  };
}
const asked = (id: number, text: string) => row(id, 'item/completed', 'userMessage', { item: { content: [{ text }] } });
const replyStarted = (id: number, uuid: string) =>
  row(id, 'item/started', 'agentMessage', { item: { id: uuid, type: 'agentMessage', text: '' } }, { item_uuid: uuid });
const replied = (id: number, uuid: string, text: string, partial = false) => row(id, 'item/completed', 'agentMessage', {
  item: { id: uuid, type: 'agentMessage', text }, completedAtMs: id, ...(partial ? { _partial: true } : {}),
}, { item_uuid: uuid });
const ended = (id: number, status: 'completed' | 'interrupted') =>
  row(id, 'turn/completed', null, { id: 'T1', status });
const streaming = (turnId: string | null, items: Record<string, string>): HarnessLiveReplies =>
  ({ turn_id: turnId, items: Object.entries(items).map(([item_id, text]) => ({ item_id, text })) });

type Gate = { hold: boolean; waiting: (() => void)[] };

function setup(servers: Record<string, CardServer>, gate: Gate = { hold: false, waiting: [] }, liveGate: Gate = { hold: false, waiting: [] }) {
  const requests: ApiRequest[] = [];
  const themeValues = new Map<string, string>();
  const transport: ApiTransportPort = {
    async send(request) {
      requests.push(request);
      const card = /\/api\/cards\/([^/]+)\//.exec(request.path)?.[1];
      const server = card === undefined ? undefined : servers[decodeURIComponent(card)];
      if (server !== undefined && request.path.includes('/harness/items')) {
        if (server.itemsFail === true) {
          /* A failure answers after a round trip, as over a network, not in the same task. */
          await new Promise((resolve) => { setTimeout(resolve, 50); });
          return { status: 503, statusText: 'Service Unavailable', body: null };
        }
        /* What the transcript says when the read is made, answered when the gate opens; a read
           past the newest page (Load earlier) gets the rows before it. */
        const newest = request.path.includes('after_id=0&');
        const limit = Number(new URL(request.path, 'http://localhost').searchParams.get('limit'));
        const filler = newest && server.earlier !== undefined
          ? Array.from({ length: limit - server.rows.length }, (_, index) => asked(index + 10, `filler ${index}`)) : [];
        const rows = [...filler, ...(newest ? server.rows : server.earlier ?? [])];
        if (gate.hold) await new Promise<void>((resolve) => { gate.waiting.push(resolve); });
        return ok(rows);
      }
      if (server !== undefined && request.path.endsWith('/harness/live')) {
        const reply = server.live;
        if (liveGate.hold) await new Promise<void>((resolve) => { liveGate.waiting.push(resolve); });
        return ok(reply);
      }
      if (server?.replace !== undefined && request.path.endsWith('/planner/input')) {
        return (request.body as { replaces_turn?: string }).replaces_turn === undefined
          ? ok({ card_id: card, worker_session_id: 'runtime' }) : server.replace();
      }
      if (server !== undefined && request.path.endsWith('/planner/run')) return ok({
        card_id: card, worker_session_id: 'runtime', phase: server.phase, model: null, reasoning_effort: null, blocked_reason: null, running_turn: server.runningTurn,
      });
      if (request.path === '/api/areas') return ok([AREA]);
      if (request.path === '/api/areas/c1/tracks') return ok([TRACK]);
      if (request.path === '/api/overlays?entity_kind=track') return ok([]);
      if (request.path === '/api/tracks/w1') return ok({
        track: TRACK, can_reopen: false, can_close: true, cards: [CARD, OTHER], overlays: [],
      });
      if (request.path === '/api/tracks/w1/conversations') return ok([OTHER_ROW]);
      if (request.path === '/api/settings') return ok({});
      return ok([]);
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{
    getItem: (key) => themeValues.get(key) ?? null, setItem: (key, value) => { themeValues.set(key, value); },
  }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { client, requests };
}

async function open(name: string) {
  const closer = screen.queryByRole('button', { name: 'Close conversation' });
  if (closer !== null) fireEvent.click(closer);
  fireEvent.click(await screen.findByRole('button', { name: new RegExp(`^Conversation ${name}`) }));
  await screen.findByRole('complementary', { name });
}

const META_HEADING = /^(Running|Completed|Interrupted|Failed|Paused)/;

/** The thread's speakers and its status row's heading, in document order. */
function threadLines(): string[] {
  return [...document.querySelectorAll<HTMLElement>('[data-nc-thread] [data-nc-turn], [data-nc-current-meta]')]
    .map((element) => element.hasAttribute('data-nc-current-meta')
      ? `[${META_HEADING.exec(element.querySelector('[data-nc-meta-state]')?.textContent?.trim() ?? '')?.[1] ?? '?'}]`
      : (element.textContent ?? '').trim());
}

/** The transcript's query, by the key the kernel's events are planned against. */
const transcriptKey = (cardId: string) => ['harness-items', cardId] as const;

/** What the kernel's `harness.phase.changed` refreshes; `transcript: false` leaves the transcript to the client. */
async function phaseChanged(client: QueryClient, cardId: string, { transcript = true } = {}) {
  await act(async () => {
    await client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) });
    if (transcript) await client.invalidateQueries({ queryKey: transcriptKey(cardId) });
  });
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/track/w1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  vi.useRealTimers();
  cleanup();
  vi.unstubAllGlobals();
});

describe('a streamed reply in the Planner conversation', () => {
  it('shows the running turn\'s text at the tail and grows it with each poll', async () => {
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Hello' }) };
    const { requests } = setup({ [CARD.id]: server });
    await open('Planner chat');
    await screen.findByText('Hello');
    expect(threadLines()).toEqual(['question', 'Hello', '[Running]']);
    server.live = streaming('T1', { m: 'Hello, world' });
    await screen.findByText('Hello, world');
    expect(threadLines()).toEqual(['question', 'Hello, world', '[Running]']);
    expect(requests.filter((request) => request.path === `/api/cards/${CARD.id}/harness/live`).length).toBeGreaterThan(1);
  });

  it('replaces the live text with the stored reply exactly once, and stops polling', async () => {
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Hello, wor' }) };
    const { client, requests } = setup({ [CARD.id]: server });
    await open('Planner chat');
    await screen.findByText('Hello, wor');
    server.rows = [...server.rows, replied(3, 'm', 'Hello, world.'), ended(4, 'completed')];
    server.live = streaming(null, {});
    server.phase = 'turn_completed';
    await phaseChanged(client, CARD.id);
    await waitFor(() => expect(threadLines()).toEqual(['question', 'Hello, world.', '[Completed]']));
    const polls = requests.filter((request) => request.path.endsWith('/harness/live')).length;
    await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 700); }); });
    expect(requests.filter((request) => request.path.endsWith('/harness/live')).length).toBe(polls);
  });

  it('draws an interrupted turn\'s partial row once, above the Interrupted line', async () => {
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Half a' }) };
    const { client } = setup({ [CARD.id]: server });
    await open('Planner chat');
    await screen.findByText('Half a');
    server.phase = 'issuing_interrupt';
    server.live = streaming('T1', { m: 'Half a rep' });
    await phaseChanged(client, CARD.id);
    await screen.findByText('Half a rep');
    server.rows = [...server.rows, replied(3, 'm', 'Half a rep', true), ended(4, 'interrupted')];
    server.live = streaming(null, {});
    server.phase = 'turn_completed';
    await phaseChanged(client, CARD.id);
    await waitFor(() => expect(threadLines()).toEqual(['question', 'Half a rep', '[Interrupted]']));
  });

  it('retires an abandoned reply at the first transcript read that started after the turn ended', async () => {
    const gate: Gate = { hold: false, waiting: [] };
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Abandoned' }) };
    const { client } = setup({ [CARD.id]: server }, gate);
    await open('Planner chat');
    await screen.findByText('Abandoned');
    /* A transcript read made while the turn was still running, answered only after it ended. */
    gate.hold = true;
    act(() => { void client.invalidateQueries({ queryKey: transcriptKey(CARD.id) }); });
    await waitFor(() => expect(gate.waiting).toHaveLength(1));
    server.rows = [...server.rows, ended(3, 'completed')];
    server.live = streaming(null, {});
    server.phase = 'turn_completed';
    /* The kernel's event refreshes the phase; the client itself re-reads the transcript. */
    await phaseChanged(client, CARD.id, { transcript: false });
    await waitFor(() => expect(gate.waiting).toHaveLength(2));
    await act(async () => { gate.waiting[0]?.(); await new Promise((resolve) => { setTimeout(resolve, 20); }); });
    expect(screen.getByText('Abandoned')).toBeTruthy();
    act(() => { gate.waiting[1]?.(); });
    await waitFor(() => expect(screen.queryByText('Abandoned')).toBeNull());
    expect(threadLines()).toEqual(['question', '[Completed]']);
  });

  it('offers Edit only once an abandoned live reply has retired, and replacing the turn brings back no live text', async () => {
    const gate: Gate = { hold: false, waiting: [] };
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Abandoned' }) };
    server.replace = () => {
      /* The kernel deletes the turn's rows, discards its live text and queues the message before it answers. */
      server.rows = [];
      server.live = streaming(null, {});
      return ok({ card_id: CARD.id, worker_session_id: 'runtime', entry_id: 'entry-1' });
    };
    const { client, requests } = setup({ [CARD.id]: server }, gate);
    await open('Planner chat');
    await screen.findByText('Abandoned');
    gate.hold = true;
    server.rows = [...server.rows, ended(3, 'completed')];
    server.live = streaming(null, {});
    server.phase = 'turn_completed';
    await phaseChanged(client, CARD.id, { transcript: false });
    await waitFor(() => expect(gate.waiting).toHaveLength(1));
    /* The live copy still closes the transcript: there is no settled outcome to edit yet. */
    expect(screen.getByText('Abandoned')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Edit message' })).toBeNull();
    gate.hold = false;
    act(() => { gate.waiting.forEach((release) => { release(); }); });
    await waitFor(() => expect(screen.queryByText('Abandoned')).toBeNull());
    fireEvent.click(await screen.findByRole('button', { name: 'Edit message' }));
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Message' }).textContent).toBe('question'));
    expect(requests.filter((request) => request.path.endsWith('/planner/input'))).toEqual([]);
    const polls = requests.filter((request) => request.path.endsWith('/harness/live')).length;
    /* Send in edit mode replaces the turn: one send that names it. */
    act(() => { fireEvent.keyDown(screen.getByRole('combobox', { name: 'Message' }), { key: 'Enter' }); });
    await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 700); }); });
    expect(screen.queryByText('Abandoned')).toBeNull();
    expect(threadLines()).toEqual(['question']);
    expect(requests.filter((request) => request.path.endsWith('/harness/live')).length).toBe(polls);
    expect(requests.filter((request) => request.path.endsWith('/planner/input')).map((request) => request.body))
      .toEqual([{ text: 'question', replaces_turn: 'T1' }]);
    expect(requests.filter((request) => request.path.endsWith('/planner/rewind'))).toEqual([]);
  });

  it('does not let a read that started before the turn ended retire the copy, however late it lands', async () => {
    const gate: Gate = { hold: false, waiting: [] };
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Hello, wor' }) };
    const { client } = setup({ [CARD.id]: server }, gate);
    await open('Planner chat');
    await screen.findByText('Hello, wor');
    /* A read whose answer the query layer already holds but has not stored yet cannot be cancelled:
       the transcript query is made deaf to cancelling, so the old read lands whatever the client does. */
    const query = client.getQueryCache().find({ queryKey: transcriptKey(CARD.id) });
    expect(query).toBeDefined();
    vi.spyOn(query!, 'cancel').mockResolvedValue(undefined);
    gate.hold = true;
    act(() => { void client.invalidateQueries({ queryKey: transcriptKey(CARD.id) }); });
    await waitFor(() => expect(gate.waiting).toHaveLength(1));
    server.rows = [...server.rows, replied(3, 'm', 'Hello, world.'), ended(4, 'completed')];
    server.live = streaming(null, {});
    server.phase = 'turn_completed';
    await phaseChanged(client, CARD.id, { transcript: false });
    await waitFor(() => expect(gate.waiting).toHaveLength(2));
    /* The read made while the turn ran lands after the phase was seen, and does not stand for it. */
    await act(async () => { gate.waiting[0]?.(); await new Promise((resolve) => { setTimeout(resolve, 20); }); });
    expect(screen.getByText('Hello, wor')).toBeTruthy();
    act(() => { gate.waiting[1]?.(); });
    await waitFor(() => expect(threadLines()).toEqual(['question', 'Hello, world.', '[Completed]']));
  });

  it('does not let Load earlier, pressed as the turn ends, retire the copy', async () => {
    const gate: Gate = { hold: false, waiting: [] };
    const server: CardServer = {
      runningTurn: null, phase: 'turn_running', rows: [asked(400, 'question'), replyStarted(401, 'm')],
      live: streaming('T1', { m: 'Hello, wor' }), earlier: [asked(1, 'long ago')],
    };
    const { client } = setup({ [CARD.id]: server }, gate);
    await open('Planner chat');
    await screen.findByText('Hello, wor');
    gate.hold = true;
    server.rows = [...server.rows, replied(402, 'm', 'Hello, world.'), ended(403, 'completed')];
    server.live = streaming(null, {});
    server.phase = 'turn_completed';
    await phaseChanged(client, CARD.id, { transcript: false });
    await waitFor(() => expect(gate.waiting).toHaveLength(1));
    /* Load earlier reads only an older page; it says nothing about the turn that just ended. */
    fireEvent.click(screen.getByRole('button', { name: 'Load earlier' }));
    await waitFor(() => expect(gate.waiting).toHaveLength(2));
    await act(async () => { gate.waiting[1]?.(); await new Promise((resolve) => { setTimeout(resolve, 20); }); });
    await screen.findByText('long ago');
    expect(screen.getByText('Hello, wor')).toBeTruthy();
    expect(screen.queryByText('Hello, world.')).toBeNull();
    /* Load earlier cancelled the client's re-read, so with no further event the client starts it again. */
    await waitFor(() => expect(gate.waiting).toHaveLength(3));
    gate.hold = false;
    act(() => { gate.waiting.forEach((release) => { release(); }); });
    await waitFor(() => expect(screen.queryByText('Hello, wor')).toBeNull());
    expect(screen.getByText('Hello, world.')).toBeTruthy();
  });

  it('stops re-reading after a failed transcript read, and keeps the reply so far', async () => {
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Hello, wor' }) };
    const { client, requests } = setup({ [CARD.id]: server });
    await open('Planner chat');
    await screen.findByText('Hello, wor');
    const reads = () => requests.filter((request) => request.path.includes('/harness/items')).length;
    const before = reads();
    /* From here every read fails after a round trip; the clock is the test's, so the window is exact. */
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval', 'Date'] });
    server.itemsFail = true;
    server.live = streaming(null, {});
    server.phase = 'turn_completed';
    await phaseChanged(client, CARD.id, { transcript: false });
    for (let step = 0; step < 50; step += 1) await act(async () => { await vi.advanceTimersByTimeAsync(100); });
    /* The one re-read the turn's end asked for failed, and is not started again while the conversation
       stays open; the next event or refetch resumes reading. */
    expect(client.getQueryState(transcriptKey(CARD.id))?.status).toBe('error');
    expect(reads()).toBe(before + 1);
    expect(screen.getByText('Hello, wor')).toBeTruthy();
  });

  it('re-reads the transcript when the turn ends before its first read has answered', async () => {
    const gate: Gate = { hold: true, waiting: [] };
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Abandoned' }) };
    const { client } = setup({ [CARD.id]: server }, gate);
    await open('Planner chat');
    await screen.findByText('Abandoned');
    expect(gate.waiting).toHaveLength(1);
    server.rows = [...server.rows, ended(3, 'completed')];
    server.live = streaming(null, {});
    server.phase = 'turn_completed';
    await phaseChanged(client, CARD.id, { transcript: false });
    /* The first read started before the turn ended, so it cannot stand for it: a second one starts. */
    await waitFor(() => expect(gate.waiting).toHaveLength(2));
    gate.hold = false;
    act(() => { gate.waiting.forEach((release) => { release(); }); });
    await waitFor(() => expect(threadLines()).toEqual(['question', '[Completed]']));
  });

  it('retires the copy of a turn that wedged without any outcome', async () => {
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Stuck' }) };
    const { client } = setup({ [CARD.id]: server });
    await open('Planner chat');
    await screen.findByText('Stuck');
    server.live = streaming(null, {});
    server.phase = 'wedged';
    await phaseChanged(client, CARD.id, { transcript: false });
    await waitFor(() => expect(screen.queryByText('Stuck')).toBeNull());
    expect(threadLines()).toEqual(['question', '[Paused]']);
  });

  it('retires the old turn\'s copy when a poll names a new turn', async () => {
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question'), replyStarted(2, 'm')], live: streaming('T1', { m: 'Old turn' }) };
    setup({ [CARD.id]: server });
    await open('Planner chat');
    await screen.findByText('Old turn');
    server.live = streaming('T2', { n: 'New turn' });
    await screen.findByText('New turn');
    expect(screen.queryByText('Old turn')).toBeNull();
    expect(threadLines()).toEqual(['question', 'New turn', '[Running]']);
  });

  it('never shows one conversation\'s live text in another', async () => {
    const first: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'first question')], live: streaming('T1', { m: 'First live' }) };
    const second: CardServer = {
      runningTurn: null, phase: 'turn_running',
      rows: [{ ...asked(1, 'second question'), card_id: OTHER.id }],
      live: streaming('T9', { m: 'Second live' }),
    };
    setup({ [CARD.id]: first, [OTHER.id]: second });
    await open('Planner chat');
    await screen.findByText('First live');
    await open('Other chat');
    await screen.findByText('Second live');
    expect(screen.queryByText('First live')).toBeNull();
    await open('Planner chat');
    await screen.findByText('First live');
    expect(screen.queryByText('Second live')).toBeNull();
  });

  it('never shows the previous turn when its first poll answers in a new streaming stretch', async () => {
    const server: CardServer = { runningTurn: null, phase: 'turn_running', rows: [asked(1, 'question')], live: streaming('T1', { m: 'Old delayed reply' }) };
    const liveGate: Gate = { hold: true, waiting: [] };
    const { client } = setup({ [CARD.id]: server }, undefined, liveGate);
    await open('Planner chat');
    await waitFor(() => expect(liveGate.waiting.length).toBeGreaterThan(0));
    server.phase = 'turn_completed';
    await phaseChanged(client, CARD.id, { transcript: false });
    await waitFor(() => expect(screen.queryByText('Old delayed reply')).toBeNull());
    server.phase = 'turn_running';
    server.live = streaming('T2', { n: 'New current reply' });
    await phaseChanged(client, CARD.id, { transcript: false });
    let resurrected = false;
    const observer = new MutationObserver(() => {
      if (threadLines().includes('Old delayed reply')) resurrected = true;
    });
    observer.observe(document.body, { childList: true, subtree: true, characterData: true });
    try {
      liveGate.hold = false;
      await act(async () => { for (const answer of liveGate.waiting.splice(0)) answer(); await Promise.resolve(); });
      await screen.findByText('New current reply');
      expect(resurrected).toBe(false);
      expect(screen.queryByText('Old delayed reply')).toBeNull();
    } finally { observer.disconnect(); }
  });

  it('drops the previous live copy as soon as the run names another turn, even without an idle phase', async () => {
    const server: CardServer = { runningTurn: { turn_id: 'T1', elapsed_ms: 100 }, phase: 'turn_running', rows: [asked(1, 'question')], live: streaming('T1', { m: 'Old identified reply' }) };
    const liveGate: Gate = { hold: false, waiting: [] };
    const { client } = setup({ [CARD.id]: server }, undefined, liveGate);
    await open('Planner chat');
    await screen.findByText('Old identified reply');
    liveGate.hold = true;
    await waitFor(() => expect(liveGate.waiting.length).toBeGreaterThan(0));
    server.runningTurn = { turn_id: 'T2', elapsed_ms: 0 };
    server.live = streaming('T2', { n: 'New identified reply' });
    await phaseChanged(client, CARD.id, { transcript: false });
    await waitFor(() => expect(screen.queryByText('Old identified reply')).toBeNull());
    liveGate.hold = false;
    await act(async () => { for (const answer of liveGate.waiting.splice(0)) answer(); await Promise.resolve(); });
    await screen.findByText('New identified reply');
    expect(screen.queryByText('Old identified reply')).toBeNull();
  });


  it('keeps matching live text when the first running identity is confirmed', async () => {
    const server: CardServer = { runningTurn: null, phase: 'issuing_turn', rows: [asked(1, 'question')], live: streaming('T1', { m: 'Already streaming' }) };
    const liveGate: Gate = { hold: false, waiting: [] };
    const { client } = setup({ [CARD.id]: server }, undefined, liveGate);
    await open('Planner chat');
    await screen.findByText('Already streaming');
    liveGate.hold = true;
    server.phase = 'turn_running';
    server.runningTurn = { turn_id: 'T1', elapsed_ms: 100 };
    await phaseChanged(client, CARD.id, { transcript: false });
    await waitFor(() => expect(liveGate.waiting.length).toBeGreaterThan(0));
    expect(screen.queryByText('Already streaming')).not.toBeNull();
    liveGate.hold = false;
    await act(async () => { for (const answer of liveGate.waiting.splice(0)) answer(); await Promise.resolve(); });
  });

  it('keeps an interrupted turn streaming when its optional running clock is absent', async () => {
    const server: CardServer = { runningTurn: { turn_id: 'T1', elapsed_ms: 100 }, phase: 'turn_running', rows: [asked(1, 'question')], live: streaming('T1', { m: 'Partial before stop' }) };
    const { client } = setup({ [CARD.id]: server });
    await open('Planner chat');
    await screen.findByText('Partial before stop');
    server.phase = 'issuing_interrupt';
    server.runningTurn = null;
    await phaseChanged(client, CARD.id, { transcript: false });
    expect(screen.queryByText('Partial before stop')).not.toBeNull();
    server.live = streaming('T1', { m: 'Partial before stop, still growing' });
    await screen.findByText('Partial before stop, still growing');
  });


  it('keeps a completed live item retired while the same turn is still running and its old poll is cached', async () => {
    const server: CardServer = { runningTurn: { turn_id: 'T1', elapsed_ms: 0 }, phase: 'turn_running', rows: [asked(1, 'question')], live: streaming('T1', { m: 'Stored during the turn' }) };
    const liveGate: Gate = { hold: false, waiting: [] };
    const { client } = setup({ [CARD.id]: server }, undefined, liveGate);
    await open('Planner chat');
    await screen.findByText('Stored during the turn');
    liveGate.hold = true;
    server.rows = [asked(1, 'question'), replied(2, 'm', 'Stored during the turn')];
    await act(async () => { await client.invalidateQueries({ queryKey: transcriptKey(CARD.id) }); });
    await waitFor(() => expect(screen.getAllByText('Stored during the turn')).toHaveLength(1));
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 50)); });
    expect(threadLines()).toEqual(['question', 'Stored during the turn', '[Running]']);
    liveGate.hold = false;
    await act(async () => { for (const answer of liveGate.waiting.splice(0)) answer(); await Promise.resolve(); });
  });

});
