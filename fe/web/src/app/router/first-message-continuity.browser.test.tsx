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
    await expect.poll(() => fixture.outbox().length).toBe(0);
    expect(bubbles()).toHaveLength(1);
    expect(Math.abs(offset() - before)).toBeLessThanOrEqual(1);
    await page.screenshot({ path: `../../../../test-results/first-message-native-canonical-${width}.png` });
  } finally { cleanup(); fixture.client.clear(); }
});
