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

function transcriptKey() { return queryKeys.harnessItems('second-planner'); }

function history(count: number, cardId = 'second-planner'): TranscriptRow[] {
  return Array.from({ length: count }, (_, index) => ({
    id: index + 1, worker_session_id: 'switch-runtime', card_id: cardId, track_id: 'daily',
    thread_id: 'switch-thread', turn_id: null, turn_error_text: null, item_uuid: null,
    item_type: 'agentMessage', method: 'item/completed', created_at_ms: index + 1,
    params: JSON.stringify({ item: { text: `Switch reply ${index}. **Evidence.**\n\n${'A real Markdown paragraph. '.repeat(8)}` } }),
  }));
}

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

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
      return request.path.includes('/second-planner/harness/items')
        ? { status: 200, statusText: 'OK', body: rows }
        : request.path.includes('/daily-planner/harness/items')
          ? { status: 200, statusText: 'OK', body: history(20, 'daily-planner') } : undefined;
    },
  });
  try {
    await expect.element(page.getByRole('button', { name: /Conversation Other conversation/ })).toBeVisible();
    await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
    await expect.poll(() => document.getElementById('conversation-daily-planner')?.querySelectorAll('[data-nc-turn="agent"]').length).toBe(20);
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    await expect.element(page.getByRole('button', { name: /Conversation Other conversation/ })).toBeVisible();
    fixture.client.setQueryData(transcriptKey(), { pages: [rows], pageParams: [0] });
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
    fixture.client.setQueryData(transcriptKey(), { pages: [rows], pageParams: [0] });
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
