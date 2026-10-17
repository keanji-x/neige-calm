import '../../styles/entry.css';
import { act, cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import type { HarnessItem } from '../../../../core/api/generated/wire.ts';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { queryKeys } from '../providers/queries.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

const history: readonly HarnessItem[] = Array.from({ length: 16 }, (_, index) => ({
  id: index + 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily',
  thread_id: 'thread', turn_id: `turn-${Math.floor(index / 2)}`, turn_error_text: null,
  item_uuid: `item-${index}`, item_type: index % 2 === 0 ? 'userMessage' : 'agentMessage', method: 'item/completed',
  params: JSON.stringify({ item: index % 2 === 0 ? { type: 'userMessage', content: [{ text: `Question ${index / 2}` }] }
    : { type: 'agentMessage', text: 'A reply with enough lines to make the conversation scroll. '.repeat(12) } }),
  created_at_ms: index + 1,
}));
const pane = () => document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
const remaining = () => pane().scrollHeight - pane().scrollTop - pane().clientHeight;
async function frames() {
  for (let index = 0; index < 4; index++) await act(async () => {
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  });
}
afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); await commands.emulateReducedMotion(false); });

it.each([
  ['earlier history', false], ['last exchange', false], ['last exchange', true],
] as const)('reveals a fresh local send after reading %s (reduced motion: %s)', async (reading, reducedMotion) => {
  await page.viewport(1400, 900);
  await commands.emulateReducedMotion(reducedMotion);
  let finish: (response: ApiTransportResponse) => void = () => {};
  const held = new Promise<ApiTransportResponse>((resolve) => { finish = resolve; });
  let rows = history;
  const { client, requests } = renderDailyFixture({ reply: (request) => {
    if (request.path.includes('/harness/items')) return { status: 200, statusText: 'OK', body: rows };
    if (request.path.endsWith('/planner/input')) return held;
    return undefined;
  } });
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.poll(() => document.querySelectorAll('[data-nc-turn="you"]').length).toBe(8);
  await frames();
  expect(remaining()).toBeLessThanOrEqual(1);
  if (reading === 'earlier history') {
    const before = pane().scrollTop;
    await commands.wheelScroll('[data-nc-drawer-scroll]', -300);
    await expect.poll(() => pane().scrollTop).toBe(before - 300);
  } else {
    await page.getByRole('button', { name: 'Jump to exchange 8: Question 7', exact: true }).click();
    await frames();
    expect(remaining()).toBeLessThanOrEqual(1);
  }
  await page.getByRole('combobox', { name: 'Message', exact: true }).click();
  await userEvent.keyboard('My fresh local message.');
  try {
    await page.getByRole('button', { name: 'Send', exact: true }).click();
    await expect.poll(() => requests.filter((request) => request.path.endsWith('/planner/input')).length).toBe(1);
    await expect.poll(() => document.querySelectorAll('[data-nc-turn="you"]').length).toBe(9);
    await frames();
    const message = page.getByText('My fresh local message.', { exact: true }).element().getBoundingClientRect();
    const viewport = pane().getBoundingClientRect();
    expect(message.top).toBeGreaterThanOrEqual(viewport.top);
    expect(message.bottom).toBeLessThanOrEqual(viewport.bottom);

    const before = pane().scrollTop;
    await commands.wheelScroll('[data-nc-drawer-scroll]', -120);
    await expect.poll(() => pane().scrollTop).toBe(before - 120);
    const parked = pane().scrollTop;
    rows = history.map((row, index) => index !== history.length - 1 ? row : {
      ...row, params: JSON.stringify({ item: { type: 'agentMessage', text: 'Background reply growth. '.repeat(100) } }),
    });
    await act(async () => { await client.refetchQueries({ queryKey: queryKeys.harnessItems('daily-planner') }); });
    await frames();
    expect(pane().scrollTop).toBe(parked);
  } finally {
    finish({ status: 409, statusText: 'Conflict', body: { code: 'planner_harness_runtime_superseded', error: 'Send refused for the visibility test.' } });
    await expect.poll(() => document.querySelectorAll('[data-nc-turn="you"]').length).toBe(8);
    const field = page.getByRole('combobox', { name: 'Message', exact: true }).element();
    await expect.poll(() => field.textContent).toBe('My fresh local message.');
    await expect.poll(() => document.activeElement).toBe(field);
  }
});

it('keeps the reading place when an existing send is retried under its same key', async () => {
  await page.viewport(1400, 900);
  let finish: (response: ApiTransportResponse) => void = () => {};
  const held = new Promise<ApiTransportResponse>((resolve) => { finish = resolve; });
  let attempts = 0;
  const rejected: ApiTransportResponse = { status: 429, statusText: 'Too Many Requests', body: { code: 'rate_limited', error: 'Wait before retrying.' } };
  const { requests } = renderDailyFixture({ reply: (request) => {
    if (request.path.includes('/harness/items')) return { status: 200, statusText: 'OK', body: history };
    if (request.path.endsWith('/planner/input')) return ++attempts === 1 ? rejected : held;
    return undefined;
  } });
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.poll(() => document.querySelectorAll('[data-nc-turn="you"]').length).toBe(8);
  await page.getByRole('combobox', { name: 'Message', exact: true }).click();
  await userEvent.keyboard('Retry these same words.');
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
  await frames();
  const before = pane().scrollTop;
  await commands.wheelScroll('[data-nc-drawer-scroll]', -300);
  await expect.poll(() => pane().scrollTop).toBe(before - 300);
  const parked = pane().scrollTop;
  try {
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await expect.poll(() => attempts).toBe(2);
    await frames();
    const posts = requests.filter((request) => request.path.endsWith('/planner/input'));
    const key = posts[0].headers?.['Idempotency-Key'];
    expect(key).toBeTruthy();
    expect(posts[1].headers?.['Idempotency-Key']).toBe(key);
    expect(pane().scrollTop).toBe(parked);
  } finally {
    finish(rejected);
    await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
  }
});
