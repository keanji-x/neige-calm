import '../../styles/entry.css';
import { act, cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import { queryKeys } from '../providers/queries.ts';
import { FIRST_MESSAGE, renderFirstMessageFixture } from './first-message-continuity-fixture.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(async () => {
  cleanup(); document.getElementById('root')?.remove();
  await commands.emulateReducedMotion(false); await page.viewport(1280, 800);
});

it.each([{ width: 1280, edited: false }, { width: 390, edited: false }, { width: 1280, edited: true }, { width: 390, edited: true }])(
  'keeps queue-only first input truthful and in place through source handoff ($width px, edited=$edited)', async ({ width, edited }) => {
  await page.viewport(width, 900); await commands.emulateReducedMotion(false);
  const fixture = renderFirstMessageFixture({ mobile: width === 390, harvestedInput: false });
  const text = edited ? 'An edited first input remains in the same place through handoff.' : FIRST_MESSAGE;
  const bubble = () => {
    const nodes = [...document.querySelectorAll<HTMLElement>('[data-nc-turn="you"]')].filter(node => node.textContent === text);
    expect(nodes).toHaveLength(1);
    return nodes[0];
  };
  const geometry = () => {
    const node = bubble();
    const scroller = node.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (scroller === null) throw new Error('The first input has no conversation scroller.');
    const box = node.getBoundingClientRect();
    return { top: box.top - scroller.getBoundingClientRect().top + scroller.scrollTop, left: box.left, width: box.width, height: box.height };
  };
  const expectGeometry = (before: ReturnType<typeof geometry>) => {
    const after = geometry();
    for (const key of ['top', 'left', 'width', 'height'] as const) expect(Math.abs(after[key] - before[key])).toBeLessThanOrEqual(1);
  };
  const frames: Array<ReturnType<typeof geometry>> = [];
  let frame: number | null = null;
  const sample = () => { frames.push(geometry()); frame = requestAnimationFrame(sample); };
  try {
    if (width === 390) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
      await page.getByRole('button', { name: '发起新对话', exact: true }).click();
    } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await page.getByRole('combobox', { name: 'Message' }).fill(FIRST_MESSAGE); await userEvent.keyboard('{Enter}');
    fixture.queueOriginal(); fixture.answerCreate(); fixture.observe('queued');
    await expect.element(page.getByRole('button', { name: 'Edit queued message', exact: true })).toBeVisible();
    await expect.poll(() => document.querySelector('[data-nc-thread]')).not.toBeNull();
    expect(document.querySelector('[data-nc-thread-empty]')).toBeNull();
    expect(document.querySelector('[data-nc-thread]')?.textContent).not.toContain('Nothing said yet.');
    const cardId = fixture.createdId(); if (cardId === null) throw new Error('No created conversation.');
    fixture.observe('unavailable');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.initialInput('daily', cardId) }); });
    if (edited) {
      await page.getByRole('button', { name: 'Edit queued message', exact: true }).click();
      await page.getByRole('combobox', { name: 'Message' }).fill(text);
      await page.getByRole('button', { name: 'Save queued message', exact: true }).click();
      await expect.poll(() => fixture.outbox().some(op => op.phase === 'initial-accepted' && op.localWrite?.text === text)).toBe(true);
      await expect.poll(() => [...document.querySelectorAll('[data-nc-size-motion]')].flatMap(node => node.getAnimations()).length).toBe(0);
    }
    const queued = geometry();
    frame = requestAnimationFrame(sample);
    fixture.clearQueue();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun(cardId) }); });
    await expect.poll(() => document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull();
    expectGeometry(queued);
    fixture.project(edited ? 1 : 0, text);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.harnessItems(cardId) }); });
    await expect.poll(() => fixture.outbox().some(op => op.phase === 'initial-accepted' && op.sourceWitness?.itemId === 7)).toBe(true);
    expectGeometry(queued);
    await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    expect(frames.length).toBeGreaterThan(1);
    for (const sampled of frames) for (const key of ['top', 'left', 'width', 'height'] as const) {
      expect(Math.abs(sampled[key] - queued[key])).toBeLessThanOrEqual(1);
    }
    await page.screenshot({ path: `../../../../test-results/first-message-stable-handoff-${width}-${edited}.png` });
  } finally { if (frame !== null) cancelAnimationFrame(frame); cleanup(); fixture.client.clear(); }
});

it.each([1280, 390])('keeps the last ordinary deleted queue row in place until its native exit finishes (%ipx)', async width => {
  await page.viewport(width, 900); await commands.emulateReducedMotion(false);
  const text = 'The last ordinary queued message leaves through its existing spring.';
  let pending = [{ entry_id: 'ordinary-last', text, rev: 0, queued_at_ms: 1 }];
  const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
  const fixture = renderDailyFixture({ reply: request => {
    if (request.method === 'DELETE' && request.path.endsWith('/planner/input/ordinary-last')) {
      pending = [];
      return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', entry_id: 'ordinary-last', rev: 0, text: null });
    }
    if (request.path.endsWith('/planner/run')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime',
      phase: 'idle', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null,
      pending, pending_overflow: 0, manual_compact_supported: true, compact_outcome: null,
      context_usage_invalidated: false, running_turn: null });
    return undefined;
  } });
  try {
    if (width === 390) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
      await page.getByRole('button', { name: '2026-10-04 Daily Planner conversation', exact: true }).click();
    } else await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Delete this message', exact: true })).toBeVisible();
    await expect.poll(() => document.querySelector('[data-nc-thread] [data-nc-pending-entry="ordinary-last"]')).not.toBeNull();
    const bubble = document.querySelector<HTMLElement>('[data-nc-pending-entry="ordinary-last"] [data-nc-turn="you"]')!;
    const exit = bubble.closest<HTMLElement>('[data-nc-collapse-presence]')!;
    const scroller = bubble.closest<HTMLElement>('[data-nc-drawer-scroll]')!;
    const offset = () => bubble.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
    const before = offset();
    const fullHeight = exit.getBoundingClientRect().height;
    const frames: Array<{ top: number; height: number; empty: boolean }> = [];
    let frame: number | null = null;
    const sample = () => {
      if (!bubble.isConnected) return;
      frames.push({ top: offset(), height: exit.getBoundingClientRect().height, empty: document.querySelector('[data-nc-thread-empty]') !== null });
      frame = requestAnimationFrame(sample);
    };
    frame = requestAnimationFrame(sample);
    try {
      await page.getByRole('button', { name: 'Delete this message', exact: true }).click();
      await expect.poll(() => bubble.isConnected).toBe(false);
    } finally { if (frame !== null) cancelAnimationFrame(frame); }
    expect(frames.some(sample => sample.height > 1 && sample.height < fullHeight - 1), JSON.stringify({ fullHeight, frames })).toBe(true);
    expect(frames.every(sample => !sample.empty && Math.abs(sample.top - before) <= 1)).toBe(true);
    await expect.poll(() => document.querySelector('[data-nc-thread-empty]')).not.toBeNull();
    await expect.element(page.getByText(width === 390 ? '聊聊你想做什么' : 'Nothing said yet.', { exact: true })).toBeVisible();
    expect(document.querySelector('[data-nc-pending-queue]')).toBeNull();
    expect(fixture.requests.filter(request => request.method === 'DELETE')).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});

it.each([1280, 390])('keeps one first bubble in place through provisional withdrawal and same-revision retry (%ipx)', async width => {
  await page.viewport(width, 900); await commands.emulateReducedMotion(false);
  const commits: number[] = []; let capture = false;
  const nodes = () => [...document.querySelectorAll<HTMLElement>('[data-nc-turn="you"]')].filter(node => node.textContent === FIRST_MESSAGE);
  const fixture = renderFirstMessageFixture({ mobile: width === 390, harvestedInput: false, holdHistory: true, holdRun: true,
    onCommit: () => { if (capture) commits.push(nodes().length); } });
  const geometry = () => {
    expect(nodes()).toHaveLength(1); const bubble = nodes()[0];
    const scroller = bubble.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (scroller === null) throw new Error('No actual first-input scroller');
    const box = bubble.getBoundingClientRect();
    return { top: box.top - scroller.getBoundingClientRect().top + scroller.scrollTop, left: box.left, width: box.width, height: box.height };
  };
  const frames: Array<ReturnType<typeof geometry>> = []; const frameCounts: number[] = []; let frame: number | null = null;
  const sample = () => { const count = nodes().length; frameCounts.push(count); if (count === 1) frames.push(geometry()); frame = requestAnimationFrame(sample); };
  try {
    if (width === 390) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
      await page.getByRole('button', { name: '发起新对话', exact: true }).click();
    } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await page.getByRole('combobox', { name: 'Message' }).fill(FIRST_MESSAGE); await userEvent.keyboard('{Enter}');
    fixture.project(0, FIRST_MESSAGE, true); fixture.observe('unavailable'); fixture.answerCreate();
    await expect.poll(() => fixture.historyCount()).toBe(1); await expect.poll(() => fixture.runCount()).toBe(1);
    act(() => { fixture.releaseHistory(0); fixture.releaseRun(0); });
    await expect.poll(() => document.querySelector('[data-nc-entry] [data-nc-turn="you"]')?.textContent).toBe(FIRST_MESSAGE);
    await expect.poll(() => [...document.querySelectorAll('[data-nc-size-motion]')].flatMap(node => node.getAnimations()).length).toBe(0);
    const cardId = fixture.createdId(); if (cardId === null) throw new Error('No actual created conversation');
    const readSource = async (kind: 'history' | 'run') => {
      const next = kind === 'history' ? fixture.historyCount() : fixture.runCount(); let read: Promise<void> | null = null;
      act(() => { read = fixture.client.invalidateQueries({ queryKey: kind === 'history' ? queryKeys.harnessItems(cardId) : queryKeys.plannerRun(cardId) }); });
      await expect.poll(() => kind === 'history' ? fixture.historyCount() : fixture.runCount()).toBe(next + 1);
      await act(async () => { if (kind === 'history') fixture.releaseHistory(next); else fixture.releaseRun(next); await read; });
    };
    const before = geometry(); capture = true; frame = requestAnimationFrame(sample);
    fixture.clearHistory(); fixture.queueOriginal(); await readSource('history'); geometry();
    await readSource('run');
    await expect.element(page.getByRole('button', { name: 'Edit queued message', exact: true })).toBeEnabled();
    expect(fixture.outbox()).toMatchObject([{ sourceWitness: null, inputFact: { rev: 0 } }]);
    fixture.clearQueue(); await readSource('run');
    await expect.poll(() => document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull(); geometry();
    fixture.project(0, FIRST_MESSAGE, true); await readSource('history');
    expect(fixture.outbox()).toMatchObject([{ sourceWitness: null }]); geometry();
    fixture.project(0); await readSource('history');
    await expect.poll(() => fixture.outbox().some(op => op.phase === 'initial-accepted' && op.sourceWitness?.turnId === 'accepted-first-turn')).toBe(true);
    geometry(); await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    expect(commits.length).toBeGreaterThan(0); expect(new Set(commits)).toEqual(new Set([1]));
    expect(frames.length).toBeGreaterThan(1); expect(new Set(frameCounts)).toEqual(new Set([1]));
    for (const sample of [...frames, geometry()]) for (const key of ['top', 'left', 'width', 'height'] as const) {
      expect(Math.abs(sample[key] - before[key])).toBeLessThanOrEqual(1);
    }
    await page.screenshot({ path: `../../../../test-results/first-message-provisional-retry-${width}.png` });
  } finally { capture = false; if (frame !== null) cancelAnimationFrame(frame); cleanup(); fixture.client.clear(); }
});

it.each([1280, 390])('keeps the ordinary queue exit spring when displayed bound history belongs to an older revision (%ipx)', async width => {
  await page.viewport(width, 900); await commands.emulateReducedMotion(false);
  const oldText = 'Actual provider-bound input at revision zero.'; const currentText = 'Later ordinary queue input at revision one.';
  let pending = [{ entry_id: 'ordinary-shared', rev: 1, text: currentText, queued_at_ms: 1 }];
  const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
  const fixture = renderDailyFixture({ reply: request => {
    if (request.path.includes('/daily-planner/harness/items')) return ok([{ id: 7, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily',
      thread_id: 'thread', turn_id: 'older-provider-turn', item_uuid: 'old-bound-row', item_type: 'userMessage', method: 'item/completed',
      params: '{}', created_at_ms: 1, turn_error_text: null, input_segments: [{ presentation: 'user', text: oldText, attachments: [],
        origin: { observation: 'user', event_id: null, queue_entry: { id: 'ordinary-shared', rev: 0 } } }] }]);
    if (request.method === 'DELETE' && request.path.endsWith('/planner/input/ordinary-shared')) {
      pending = []; return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', entry_id: 'ordinary-shared', rev: 1, text: null });
    }
    if (request.path.endsWith('/planner/run')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'idle', model: null,
      reasoning_effort: null, permission_mode: 'never', blocked_reason: null, pending, pending_overflow: 0,
      manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false, running_turn: null });
    return undefined;
  } });
  try {
    if (width === 390) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
      await page.getByRole('button', { name: '2026-10-04 Daily Planner conversation', exact: true }).click();
    } else await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Delete this message', exact: true })).toBeEnabled();
    await expect.element(page.getByText(oldText, { exact: true })).toBeVisible();
    const bubble = document.querySelector<HTMLElement>('[data-nc-pending-entry="ordinary-shared"] [data-nc-turn="you"]');
    if (bubble === null) throw new Error('The real ordinary queue row was not mounted.');
    const exit = bubble.closest<HTMLElement>('[data-nc-collapse-presence]');
    if (exit === null) throw new Error('The ordinary row has no existing presence owner.');
    const fullHeight = exit.getBoundingClientRect().height; const heights: number[] = []; let frame: number | null = null;
    const sample = () => { if (!bubble.isConnected) return; heights.push(exit.getBoundingClientRect().height); frame = requestAnimationFrame(sample); };
    frame = requestAnimationFrame(sample);
    try {
      await page.getByRole('button', { name: 'Delete this message', exact: true }).click();
      await expect.poll(() => bubble.isConnected).toBe(false);
    } finally { if (frame !== null) cancelAnimationFrame(frame); }
    expect(heights.some(height => height > 1 && height < fullHeight - 1), JSON.stringify({ fullHeight, heights })).toBe(true);
    await expect.element(page.getByText(oldText, { exact: true })).toBeVisible();
    expect(fixture.requests.filter(request => request.method === 'DELETE')).toHaveLength(1);
  } finally { cleanup(); fixture.client.clear(); }
});
