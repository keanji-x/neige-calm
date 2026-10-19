import { historyRows, latestHistory, observedWindow, type History } from '../conversations/history.ts';
import '../../styles/entry.css';
import { act, cleanup, fireEvent, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands, page } from 'vitest/browser';
import type { HarnessItem as TranscriptRow } from '../../../../core/api/generated/wire.ts';
import { queryKeys } from '../providers/queries.ts';
import { createReadReceiptsKernel } from '../providers/read-receipts-fixture.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

declare module 'vitest/internal/browser' {
  interface BrowserCommands { recordConversationSwitchPerformance(count: number, report: string): Promise<void> }
}

// Existing server boundary, checked against the actual presentation requests below.
const PAGE_LIMIT = 60;

function transcriptKey(cardId = 'second-planner') { return queryKeys.harnessItems(cardId); }

function history(count: number, cardId = 'second-planner'): TranscriptRow[] {
  return Array.from({ length: count }, (_, index) => ({
    id: index + 1, worker_session_id: 'switch-runtime', card_id: cardId, track_id: 'daily',
    thread_id: 'switch-thread', turn_id: null, turn_error_text: null, item_uuid: null,
    item_type: 'agentMessage', method: 'item/completed', created_at_ms: index + 1,
    params: JSON.stringify({ item: { text: `Switch reply ${index}. **Evidence.**\n\n${'A real Markdown paragraph. '.repeat(8)}` } }),
  }));
}

// Match the HTTP range contract: exclusive cursors, ascending response rows,
// and a budget applied from the requested end (never a cache/window merge).
function transcriptPage(rows: TranscriptRow[], path: string): TranscriptRow[] {
  const params = new URL(path, 'http://fixture').searchParams;
  const cursor = Number(params.get('after_id') ?? 0);
  const limit = Math.min(500, Math.max(0, Number(params.get('limit') ?? 100)));
  if (limit === 0) return [];
  return params.get('direction') === 'desc'
    ? rows.filter(row => cursor === 0 || row.id < cursor).slice(-limit)
    : rows.filter(row => row.id > cursor).slice(0, limit);
}

it('serves budgeted transcript ranges and terminal empty reads in server order', () => {
  const rows = history(900);
  const read = (cursor: number, direction: 'asc' | 'desc' = 'desc') =>
    transcriptPage(rows, `/api/cards/second-planner/harness/items?after_id=${cursor}&direction=${direction}&limit=${PAGE_LIMIT}`).map(row => row.id);
  expect(read(0)).toEqual(rows.slice(-PAGE_LIMIT).map(row => row.id));
  expect(read(841)).toEqual(rows.slice(780, 840).map(row => row.id));
  expect(read(61)).toEqual(rows.slice(0, 60).map(row => row.id));
  expect(read(21)).toEqual(rows.slice(0, 20).map(row => row.id));
  expect(read(1)).toEqual([]);
  expect(read(840, 'asc')).toEqual(rows.slice(840).map(row => row.id));
  expect(read(900, 'asc')).toEqual([]);
  expect(transcriptPage(rows, '/harness/items?after_id=841&direction=desc&limit=3').map(row => row.id)).toEqual([838, 839, 840]);
  expect(transcriptPage(rows, '/harness/items?after_id=0&direction=desc&limit=0')).toEqual([]);
});

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it.each([false, true])('keeps expanded Track A → B → A idle without earlier reads (cached earlier page: %s)', async cachedEarlier => {
  await page.viewport(1400, 900);
  const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
  // Two fixed fixture windows straddle the existing 60-row server boundary.
  let refreshed = false;
  const newest = (cardId: string) => history(PAGE_LIMIT, cardId).map(row => ({ ...row,
    id: row.id + PAGE_LIMIT, track_id: cardId === 'daily-planner' ? 'daily' : 'project',
  }));
  let release!: () => void;
  let waiting = new Promise<void>(resolve => { release = resolve; });
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', reply: async request => {
    if (request.path === '/api/tracks/project') return ok({
      track: { workspace: { isolation: 'managed', kind: 'managed', path: '/tmp', frozen_at: null }, id: 'project', area_id: 'project-area', title: 'Project evidence', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 },
      can_reopen: false, can_admin: false, can_close: true,
      cards: [{ id: 'project-planner', track_id: 'project', kind: 'codex', title: 'Project conversation', sort: 1,
        payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 }], overlays: [],
    });
    if (!request.path.includes('/harness/items')) return undefined;
    await waiting;
    const cardId = request.path.includes('/daily-planner/') ? 'daily-planner' : 'project-planner';
    const cursor = new URL(request.path, 'http://fixture').searchParams.get('after_id');
    const latest = newest(cardId);
    if (refreshed && cardId === 'daily-planner') latest.push({ ...latest.at(-1)!, id: 121, created_at_ms: 121,
      params: JSON.stringify({ item: { text: 'Fresh reply after returning to A' } }) });
    return ok(cursor === '0' ? latest.slice(-PAGE_LIMIT) : history(PAGE_LIMIT, cardId).map(row => ({ ...row,
      track_id: cardId === 'daily-planner' ? 'daily' : 'project',
    })));
  } });
  const reads = () => fixture.requests.filter(request => request.path.includes('/harness/items'));
  const earlier = () => reads().filter(request => new URL(request.path, 'http://fixture').searchParams.get('after_id') !== '0');
  const evidence: Array<{ phase: string; paths: string[] }> = [];
  const settle = async (phase: string) => {
    // Include presentation frames, idle work and the reader-input lease; supply no scroll input.
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 350)); });
    evidence.push({ phase, paths: reads().map(request => request.path) });
    await commands.recordConversationSwitchPerformance(cachedEarlier ? PAGE_LIMIT * 2 : PAGE_LIMIT,
      JSON.stringify({ scenario: 'expanded Track switch without scrolling', cachedEarlier, earlierRequests: earlier().length, evidence }));
    expect(earlier(), JSON.stringify(evidence)).toHaveLength(0);
  };
  const navigate = async (trackId: string) => {
    await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId }, search: { panel: 'conversations' } }); });
  };
  try {
    await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
    await expect.element(page.getByText('Loading conversation…')).toBeVisible();
    await settle('A waiting');
    release();
    await expect.poll(() => document.querySelectorAll('#conversation-daily-planner [data-nc-turn="agent"]').length).toBe(PAGE_LIMIT);
    await settle('A initial layout');
    waiting = new Promise<void>(resolve => { release = resolve; });
    await navigate('project');
    await page.getByRole('button', { name: 'Conversation Project conversation', exact: true }).click();
    await expect.element(page.getByText('Loading conversation…')).toBeVisible();
    await settle('B waiting');
    release();
    await expect.poll(() => document.querySelectorAll('#conversation-project-planner [data-nc-turn="agent"]').length).toBe(PAGE_LIMIT);
    await settle('B initial layout');
    if (cachedEarlier) fixture.client.setQueryData(transcriptKey('daily-planner'), {
      latest: observedWindow(newest('daily-planner'), { kind: 'latest' }),
      retained: [observedWindow(history(PAGE_LIMIT, 'daily-planner'), { kind: 'before', rowId: PAGE_LIMIT + 1 })],
    });
    waiting = new Promise<void>(resolve => { release = resolve; });
    refreshed = true;
    await navigate('daily');
    await expect.element(page.getByRole('complementary', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
    await expect.poll(() => document.querySelectorAll('#conversation-daily-planner [data-nc-turn="agent"]').length).toBe(PAGE_LIMIT * (cachedEarlier ? 2 : 1));
    const retained = document.querySelector('#conversation-daily-planner [data-nc-turn="agent"]');
    release();
    await expect.element(page.getByText('Fresh reply after returning to A', { exact: true })).toBeVisible();
    expect(document.querySelector('#conversation-daily-planner [data-nc-turn="agent"]')).toBe(retained);
    expect(reads().filter(request => request.path.includes('/daily-planner/') && new URL(request.path, 'http://fixture').searchParams.get('after_id') === '0')).toHaveLength(2);
    await settle('A cached return layout');
    // B also retains its expanded pane when revisited; neither visit authorizes history.
    await navigate('project');
    await expect.element(page.getByRole('complementary', { name: 'Project conversation', exact: true })).toBeVisible();
    await settle('B retained return');
    expect(reads().some(request => request.path.includes('/daily-planner/'))).toBe(true);
    expect(reads().some(request => request.path.includes('/project-planner/'))).toBe(true);
  } finally { release(); cleanup(); fixture.client.clear(); }
});

it.each([20, 900])('presents the selected shell before mounting %i cached messages', async count => {
  await page.viewport(1400, 900);
  const rows = history(count);
  const commits: Array<{ at: number; thread: boolean; loading: boolean }> = [];
  const receipts = createReadReceiptsKernel({ baselineMs: 0 });
  let firstFrame: { thread: boolean; read: boolean } | null = null;
  let firstFrameAt: number | null = null;
  let observing = false;
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, receipts,
    onCommit: () => {
      if (!observing) return;
      const drawer = document.getElementById('conversation-second-planner');
      if (drawer === null) return;
      commits.push({ at: performance.now(), thread: drawer.querySelector('[data-nc-thread]') !== null,
        loading: drawer.textContent?.includes('Loading conversation…') === true });
      if (commits.length === 1) requestAnimationFrame(() => {
        firstFrameAt = performance.now();
        firstFrame = { thread: drawer.querySelector('[data-nc-thread]') !== null,
          read: receipts.stored('conversation', 'second-planner') !== undefined };
      });
    },
    reply: request => {
      if (request.path === '/api/tracks/daily/conversations') return { status: 200, statusText: 'OK', body: [{
        id: 'second-planner', trackId: 'daily', title: 'Other conversation', kind: 'track-assistant',
        provider: 'codex', state: 'idle', updatedAt: count, archivedAt: null, lastTurnCompletedAt: count,
      }] };
      if (request.path.includes('/harness/items')) {
        expect(new URL(request.path, 'http://fixture').searchParams.get('limit')).toBe(String(PAGE_LIMIT));
      }
      return request.path.includes('/second-planner/harness/items')
        ? { status: 200, statusText: 'OK', body: transcriptPage(rows, request.path) }
        : request.path.includes('/daily-planner/harness/items')
          ? { status: 200, statusText: 'OK', body: transcriptPage(history(20, 'daily-planner'), request.path) } : undefined;
    },
  });
  try {
    await expect.element(page.getByRole('button', { name: /Conversation Other conversation/ })).toBeVisible();
    await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
    await expect.poll(() => document.getElementById('conversation-daily-planner')?.querySelectorAll('[data-nc-turn="agent"]').length).toBe(20);
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    await expect.element(page.getByRole('button', { name: /Conversation Other conversation/ })).toBeVisible();
    const latest = observedWindow(rows.slice(-PAGE_LIMIT), { kind: 'latest' });
    const older = rows.slice(0, -PAGE_LIMIT);
    const cached: History = { latest, retained: older.length === 0 ? [] : [{
      // Compacted full earlier responses: 1..840, each read below the next
      // window's exclusive bound. A full oldest page does not prove start.
      rows: older, coverage: { lower: { kind: 'from', rowId: older[0].id },
        upper: { kind: 'before', rowId: latest.rows[0].id } },
    }] };
    fixture.client.setQueryData(transcriptKey(), cached);
    expect(cached.latest.rows).toHaveLength(Math.min(count, PAGE_LIMIT));
    expect(historyRows(cached)).toEqual(rows);
    const start = performance.now();
    observing = true;
    fireEvent.click(screen.getByRole('button', { name: /Conversation Other conversation/ }));
    await expect.element(page.getByText(`Switch reply ${count - 1}.`, { exact: false })).toBeVisible();
    const presented = commits.find(commit => commit.thread);
    await commands.recordConversationSwitchPerformance(count, JSON.stringify({ count,
      shellCommitMs: commits[0].at - start, firstFrameMs: firstFrameAt === null ? null : firstFrameAt - start,
      bodyCommitMs: (presented?.at ?? start) - start,
      firstCommitIncludedBody: commits[0].thread, firstFrame, domElements: document.querySelectorAll('*').length }));
    // A cache hit must still give the shell its own commit before the expensive body.
    expect(commits[0]).toMatchObject({ thread: false, loading: true });
    expect(presented).toBeDefined();
    expect(firstFrame).toEqual({ thread: false, read: false });
    await expect.poll(() => receipts.stored('conversation', 'second-planner')?.readThroughMs).toBe(count);
    // Reopening still performs a real latest refresh; the budgeted response
    // must preserve all loaded history and all mounted message bodies.
    await expect.poll(() => fixture.requests.filter(request => request.path.includes('/second-planner/harness/items')
      && new URL(request.path, 'http://fixture').searchParams.get('after_id') === '0').length).toBe(1);
    await expect.poll(() => fixture.client.getQueryState(transcriptKey())?.fetchStatus).toBe('idle');
    expect(fixture.client.getQueryState(transcriptKey())).toMatchObject({ status: 'success', error: null });
    const refreshed = fixture.client.getQueryData<History>(transcriptKey())!;
    expect(refreshed).not.toBe(cached);
    expect(refreshed.latest.rows).toEqual(rows.slice(-PAGE_LIMIT));
    expect(historyRows(refreshed)).toEqual(rows);
    expect(document.querySelectorAll('#conversation-second-planner [data-nc-turn="agent"]')).toHaveLength(count);
    if (count === 900) await page.screenshot({ path: './__screenshots__/conversation-presentation-desktop.png' });
  } finally { observing = false; cleanup(); fixture.client.clear(); }
});

it.each([390, 1400])('can close at the first shell frame before cached content mounts (%ipx)', async width => {
  await page.viewport(width, 900);
  const rows = history(20);
  let scheduled = false;
  let frame: { thread: boolean; closed: boolean } | null = null;
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true,
    onCommit: () => {
      const drawer = document.getElementById('conversation-second-planner');
      if (scheduled || drawer === null) return;
      scheduled = true;
      requestAnimationFrame(() => {
        const close = drawer.querySelector<HTMLButtonElement>('button[aria-label="Close conversation"]');
        frame = { thread: drawer.querySelector('[data-nc-thread]') !== null, closed: close !== null };
        if (close !== null) fireEvent.click(close);
      });
    },
    reply: request => request.path.includes('/second-planner/harness/items')
      ? { status: 200, statusText: 'OK', body: rows } : undefined,
  });
  try {
    await expect.element(page.getByRole('button', { name: /Conversation Other conversation/ })).toBeVisible();
    fixture.client.setQueryData(transcriptKey(), latestHistory(undefined, rows));
    fireEvent.click(screen.getByRole('button', { name: /Conversation Other conversation/ }));
    await expect.poll(() => frame).toEqual({ thread: false, closed: true });
    await expect.poll(() => document.getElementById('conversation-second-planner')).toBeNull();
    expect(screen.queryByText(/Switch reply/)).toBeNull();
  } finally { cleanup(); fixture.client.clear(); }
});

it('lets a slow history read finish after closing without painting its body', async () => {
  const rows = history(1);
  let release!: () => void;
  const pending = new Promise<void>(resolve => { release = resolve; });
  const receipts = createReadReceiptsKernel({ baselineMs: 0 });
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, receipts,
    reply: async request => {
      if (request.path.includes('/second-planner/harness/items')) {
        await pending;
        return { status: 200, statusText: 'OK', body: rows };
      }
      return undefined;
    },
  });
  try {
    await page.getByRole('button', { name: /Conversation Other conversation/ }).click();
    await expect.element(page.getByText('Loading conversation…')).toBeVisible();
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    await expect.poll(() => document.getElementById('conversation-second-planner')).toBeNull();
    await act(async () => { release(); await pending; });
    await expect.poll(() => fixture.client.getQueryState(transcriptKey())?.status).toBe('success');
    expect(document.getElementById('conversation-second-planner')).toBeNull();
    expect(screen.queryByText(/Switch reply 0/)).toBeNull();
    expect(receipts.posts().filter(request => request.path.includes('/second-planner/'))).toHaveLength(0);
  } finally { release(); cleanup(); fixture.client.clear(); }
});

it('gives a late history result its own rendering opportunity before first mounting', async () => {
  const rows = history(20);
  let release!: () => void;
  const pending = new Promise<void>(resolve => { release = resolve; });
  const commits: boolean[] = [];
  let observing = false;
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true,
    onCommit: () => {
      if (!observing || fixture.client.getQueryData(transcriptKey()) === undefined) return;
      const drawer = document.getElementById('conversation-second-planner');
      if (drawer !== null) commits.push(drawer.querySelector('[data-nc-thread]') !== null);
    },
    reply: async request => {
      if (!request.path.includes('/second-planner/harness/items')) return undefined;
      await pending;
      return { status: 200, statusText: 'OK', body: rows };
    },
  });
  try {
    await page.getByRole('button', { name: /Conversation Other conversation/ }).click();
    await expect.element(page.getByText('Loading conversation…')).toBeVisible();
    // The shell has already had time to present while the history is still unknown.
    for (let index = 0; index < 4; index++) {
      await act(async () => { await new Promise<void>(resolve => requestAnimationFrame(() => resolve())); });
    }
    observing = true;
    await act(async () => { release(); await pending; });
    await expect.element(page.getByText('Switch reply 19.', { exact: false })).toBeVisible();
    expect(commits[0]).toBe(false);
    expect(commits).toContain(true);
  } finally { observing = false; release(); cleanup(); fixture.client.clear(); }
});
