import '../../styles/entry.css';
import { act, cleanup } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import { FIRST_MESSAGE, renderFirstMessageFixture } from './first-message-continuity-fixture.tsx';

afterEach(async () => {
  cleanup(); vi.unstubAllGlobals(); document.getElementById('root')?.remove();
  await commands.emulateReducedMotion(false); await page.viewport(1280, 800);
});

it.each([{ width: 1280, reduce: false }, { width: 390, reduce: false }, { width: 390, reduce: true }])(
  'keeps one readable first message across real draft/preview/thread commits ($width px, reduced=$reduce)', async ({ width, reduce }) => {
    await page.viewport(width, 900); await commands.emulateReducedMotion(reduce);
    const occurrences = () => [...document.querySelectorAll('[data-nc-turn="you"]')]
      .filter(element => element.textContent === FIRST_MESSAGE);
    let sawWords = false;
    const commits: number[] = [];
    const fixture = renderFirstMessageFixture({ mobile: width === 390, onCommit: () => {
      const count = occurrences().length;
      if (count > 0) sawWords = true;
      if (sawWords) commits.push(count);
    } });
    try {
      if (width === 390) {
        await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
        await page.getByRole('button', { name: '发起新对话', exact: true }).click();
      } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
      const field = page.getByRole('combobox', { name: 'Message' });
      await field.fill(FIRST_MESSAGE); await userEvent.keyboard('{Enter}');
      await expect.poll(() => occurrences().length).toBe(1);
      const frames = new Map<number, FrameRequestCallback>();
      let frameId = 0;
      vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { frames.set(++frameId, callback); return frameId; });
      vi.stubGlobal('cancelAnimationFrame', (id: number) => { frames.delete(id); });
      fixture.answerCreate();
      await expect.poll(() => fixture.inspectionCount()).toBeGreaterThan(0);
      await expect.element(page.getByText('Checking the first message’s current status.', { exact: true })).toBeVisible();
      expect(document.querySelector('[data-nc-thread]')).toBeNull();
      expect(occurrences()).toHaveLength(1);
      const box = occurrences()[0].getBoundingClientRect();
      expect(box.width).toBeGreaterThan(50); expect(box.height).toBeGreaterThan(10);
      expect(box.left).toBeGreaterThanOrEqual(0); expect(box.right).toBeLessThanOrEqual(width);
      expect(box.top).toBeGreaterThanOrEqual(0); expect(box.bottom).toBeLessThanOrEqual(900);
      await page.screenshot({ path: `../../../../test-results/first-message-preview-${width}-${reduce}.png` });
      fixture.project(); fixture.observe('projected');
      await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', fixture.createdId()] }); });
      expect(occurrences()).toHaveLength(1);
      for (let frame = 0; frame < 2; frame++) {
        const callbacks = [...frames.values()]; frames.clear();
        await act(async () => { callbacks.forEach(callback => callback(0)); await Promise.resolve(); });
      }
      await expect.poll(() => document.querySelector('[data-nc-thread]')).not.toBeNull();
      await page.screenshot({ path: `../../../../test-results/first-message-thread-${width}-${reduce}.png` });
      expect(occurrences()).toHaveLength(1);
      expect(commits.length).toBeGreaterThan(2); expect(new Set(commits)).toEqual(new Set([1]));
      expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations'))).toHaveLength(1);
      expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(0);
      expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    } finally { cleanup(); fixture.client.clear(); }
  },
);

it.each([1280, 390])('keeps the first bubble in place through native admission with held reads (%ipx)', async width => {
  await page.viewport(width, 900);
  await commands.emulateReducedMotion(false);
  const fixture = renderFirstMessageFixture({ mobile: width === 390, holdHistory: true, holdRun: true, harvestedInput: false });
  const bubbles = () => [...document.querySelectorAll<HTMLElement>('[data-nc-turn="you"]')].filter(node => node.textContent === FIRST_MESSAGE);
  const offset = () => {
    const bubble = bubbles()[0];
    const scroller = bubble?.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (bubble === undefined || scroller == null) throw new Error('The first bubble has no mounted conversation scroller.');
    return bubble.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
  };
  try {
    if (width === 390) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
      await page.getByRole('button', { name: '发起新对话', exact: true }).click();
    } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await page.getByRole('combobox', { name: 'Message' }).fill(FIRST_MESSAGE);
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => bubbles().length).toBe(1);
    await page.screenshot({ path: `../../../../test-results/first-message-native-held-create-${width}.png` });
    const before = offset();
    fixture.answerCreate();
    await expect.poll(() => document.querySelector('[data-nc-thread]')).not.toBeNull();
    await expect.poll(() => fixture.historyCount()).toBeGreaterThan(0);
    await expect.poll(() => fixture.runCount()).toBeGreaterThan(0);
    expect(bubbles()).toHaveLength(1);
    expect(Math.abs(offset() - before)).toBeLessThanOrEqual(1);
    await expect.element(page.getByText('Loading conversation…', { exact: true })).not.toBeInTheDocument();
    await expect.element(page.getByText('Checking the first message’s current status.', { exact: true })).not.toBeInTheDocument();
    await expect.element(page.getByRole('button', { name: 'Status unconfirmed', expanded: false })).toBeVisible();
    await page.screenshot({ path: `../../../../test-results/first-message-native-accepted-held-${width}.png` });
    fixture.project(); fixture.observe('projected');
    await expect.poll(() => fixture.historyCount()).toBeGreaterThan(1);
    await expect.poll(() => fixture.runCount()).toBeGreaterThan(1);
    await act(async () => {
      for (let index = 0; index < fixture.historyCount(); index++) fixture.releaseHistory(index);
      for (let index = 0; index < fixture.runCount(); index++) fixture.releaseRun(index);
      await Promise.resolve();
    });
    await expect.poll(() => fixture.outbox().some(op => op.phase === 'initial-accepted' && op.sourceWitness?.itemId === 7)).toBe(true);
    expect(bubbles()).toHaveLength(1);
    expect(Math.abs(offset() - before)).toBeLessThanOrEqual(1);
    await page.screenshot({ path: `../../../../test-results/first-message-native-canonical-${width}.png` });
  } finally { cleanup(); fixture.client.clear(); }
});

it.each([1280, 390])('keeps real first-input queue controls readable while history is held (%ipx)', async width => {
  await page.viewport(width, 900);
  await commands.emulateReducedMotion(false);
  const fixture = renderFirstMessageFixture({ mobile: width === 390, holdHistory: true, holdRun: true });
  try {
    if (width === 390) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
      await page.getByRole('button', { name: '发起新对话', exact: true }).click();
    } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await page.getByRole('combobox', { name: 'Message' }).fill(FIRST_MESSAGE);
    await userEvent.keyboard('{Enter}');
    fixture.queueOriginal();
    fixture.answerCreate();
    await expect.poll(() => document.querySelector('[data-nc-thread]')).not.toBeNull();
    await expect.poll(() => fixture.runCount()).toBe(1);
    act(() => { fixture.releaseRun(0); });
    await expect.poll(() => document.querySelector('[data-nc-pending-entry="first-entry"]')?.textContent).toContain(FIRST_MESSAGE);
    expect(document.querySelector('[data-nc-pending-entry="first-entry"] [aria-label="Delete this message"]')).not.toBeNull();
    await expect.element(page.getByRole('button', { name: 'Edit queued message', exact: true })).toBeVisible();
    await expect.element(page.getByText('Loading conversation…', { exact: true })).not.toBeInTheDocument();
    const bubble = document.querySelector<HTMLElement>('[data-nc-pending-entry="first-entry"] [data-nc-turn="you"]');
    expect(bubble).not.toBeNull();
    const box = bubble!.getBoundingClientRect();
    expect(box.left).toBeGreaterThanOrEqual(0); expect(box.right).toBeLessThanOrEqual(width);
    await page.screenshot({ path: `../../../../test-results/first-message-native-queue-held-${width}.png` });
  } finally { cleanup(); fixture.client.clear(); }
});

it.each([{ width: 1280, edited: false }, { width: 390, edited: false }, { width: 1280, edited: true }, { width: 390, edited: true }])(
  'hands the real first-input queue to one canonical bubble ($width px, edited=$edited)', async ({ width, edited }) => {
  await page.viewport(width, 900);
  await commands.emulateReducedMotion(false);
  const fixture = renderFirstMessageFixture({ mobile: width === 390 });
  const canonical = edited ? 'Edited canonical first input after the real queue drains.' : FIRST_MESSAGE;
  const originalWords = () => [...document.querySelectorAll('[data-nc-turn="you"]')].filter(node => node.textContent === FIRST_MESSAGE);
  try {
    if (width === 390) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
      await page.getByRole('button', { name: '发起新对话', exact: true }).click();
    } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await page.getByRole('combobox', { name: 'Message' }).fill(FIRST_MESSAGE);
    await userEvent.keyboard('{Enter}');
    fixture.queueOriginal(); fixture.answerCreate(); fixture.observe('queued');
    await expect.poll(() => document.querySelector('[data-nc-pending-entry="first-entry"]')?.textContent).toContain(FIRST_MESSAGE);
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    fixture.observe('unavailable');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['conversation-initial-input', 'daily', cardId] }); });
    fixture.clearQueue(); fixture.project(canonical);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['harness-items', cardId] }); });
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', cardId] }); });
    await expect.poll(() => [...document.querySelectorAll('[data-nc-entry] [data-nc-turn="you"]')]
      .filter(node => node.textContent === canonical).length).toBe(1);
    const drawn = [...document.querySelectorAll('[data-nc-turn="you"]')].filter(node => node.textContent === canonical);
    const original = originalWords();
    await page.screenshot({ path: `../../../../test-results/first-message-native-canonical-handoff-${width}-${edited}.png` });
    expect(drawn).toHaveLength(1);
    expect(original).toHaveLength(edited ? 0 : 1);
    expect(fixture.outbox()).toMatchObject([{ phase: 'initial-accepted' }]);
  } finally { cleanup(); fixture.client.clear(); }
});


it.each([{ width: 1280, steer: false }, { width: 390, steer: false }, { width: 1280, steer: true }, { width: 390, steer: true }])(
  'keeps the acknowledged local first-input queue write truthful ($width px, steer=$steer)', async ({ width, steer }) => {
    await page.viewport(width, 900); await commands.emulateReducedMotion(false);
    const fixture = renderFirstMessageFixture({ mobile: width === 390, running: steer });
    try {
      if (width === 390) {
        await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
        await page.getByRole('button', { name: '发起新对话', exact: true }).click();
      } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
      await page.getByRole('combobox', { name: 'Message' }).fill(FIRST_MESSAGE); await userEvent.keyboard('{Enter}');
      if (!steer) fixture.project(FIRST_MESSAGE, true);
      fixture.queueOriginal(); fixture.answerCreate(); fixture.observe('queued');
      await expect.poll(() => document.querySelector('[data-nc-pending-entry="first-entry"]')?.textContent).toContain(FIRST_MESSAGE);
      const cardId = fixture.createdId();
      if (cardId === null) throw new Error('No actual created conversation');
      await expect.poll(() => fixture.client.getQueriesData<{ observation: { kind: string } }>({ queryKey: ['conversation-initial-input', 'daily', cardId] })
        .some(([, value]) => value?.observation.kind === 'queued')).toBe(true);
      fixture.observe('unavailable');
      await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['conversation-initial-input', 'daily', cardId] }); });
      await page.getByRole('button', { name: steer ? 'Say it now' : 'Delete this message', exact: true }).click();
      await expect.poll(() => document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull();
      const copies = [...document.querySelectorAll('[data-nc-turn="you"]')].filter(node => node.textContent === FIRST_MESSAGE).length;
      await page.screenshot({ path: `../../../../test-results/first-message-local-write-${width}-${steer}.png` });
      expect(copies).toBe(steer ? 1 : 0);
    } finally { cleanup(); fixture.client.clear(); }
  },
);


it.each([1280, 390])('keeps one accepted fallback when a real queue update arrives before canonical input (%ipx)', async width => {
  await page.viewport(width, 900); await commands.emulateReducedMotion(false);
  const fixture = renderFirstMessageFixture({ mobile: width === 390 });
  try {
    if (width === 390) {
      await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
      await page.getByRole('button', { name: '发起新对话', exact: true }).click();
    } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
    await page.getByRole('combobox', { name: 'Message' }).fill(FIRST_MESSAGE); await userEvent.keyboard('{Enter}');
    fixture.queueOriginal(); fixture.answerCreate(); fixture.observe('queued');
    await expect.element(page.getByRole('button', { name: 'Delete this message', exact: true })).toBeEnabled();
    const cardId = fixture.createdId();
    if (cardId === null) throw new Error('No actual created conversation');
    fixture.observe('unavailable');
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['conversation-initial-input', 'daily', cardId] }); });
    fixture.clearQueue();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: ['planner-run', cardId] }); });
    await expect.poll(() => document.querySelector('[data-nc-pending-entry="first-entry"]')).toBeNull();
    const count = [...document.querySelectorAll('[data-nc-turn="you"]')].filter(node => node.textContent === FIRST_MESSAGE).length;
    await page.screenshot({ path: `../../../../test-results/first-message-fallback-before-canonical-${width}.png` });
    expect(count).toBe(1);
  } finally { cleanup(); fixture.client.clear(); }
});
