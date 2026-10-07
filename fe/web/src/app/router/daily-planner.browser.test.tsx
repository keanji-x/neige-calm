import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it('reads a daily plan and yesterday’s report evidence in a real browser', async () => {
  await page.viewport(1440, 900);
  const { requests } = renderDailyFixture();
  await expect.element(page.getByRole('button', { name: 'Rename track' })).toHaveTextContent('2026-10-04');
  await expect.element(page.getByRole('button', { name: /Conversation Daily Planner conversation/ })).toBeVisible();
  await page.getByText('Report changes').click();
  const source = page.getByRole('heading', { name: /Project evidence.*2 report edits/ });
  await expect.element(source).toBeVisible();
  const marker = source.element().querySelector('span:first-child')!;
  expect(getComputedStyle(marker).rotate).toBe('0deg');
  await source.click();
  await expect.poll(() => getComputedStyle(marker).rotate).toBe('90deg');
  await page.getByText('Individual edits').click();
  await page.getByRole('heading', { name: /Report edit.*09:00:00/ }).click();
  await page.getByText('After', { exact: true }).click();
  await expect.element(page.getByRole('heading', { name: 'After release' })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
  expect(requests.every((request) => request.method === 'GET')).toBe(true);
  await page.screenshot({ path: './__screenshots__/daily-planner-desktop.png' });
});


it.each([1000, 768])('keeps the homepage calendar and full-height daily Planner usable open at height %s', async (height) => {
  await page.viewport(1440, height);
  const { requests } = renderDailyFixture();
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
  const drawer = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(drawer).toBeVisible();
  await expect.element(calendar).toBeVisible();
  expect(calendar.element().getBoundingClientRect().right).toBeLessThanOrEqual(drawer.element().getBoundingClientRect().left);
  await calendar.getByRole('link', { name: /October 3, 2026/ }).click();
  await expect.element(page.getByRole('heading', { name: 'Sat, Oct 3' })).toBeVisible();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await expect.element(calendar).toBeVisible();
  expect(calendar.element().getBoundingClientRect().right).toBeLessThanOrEqual(drawer.element().getBoundingClientRect().left);
  expect(page.getByRole('combobox', { name: 'Message', exact: true }).element().getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  expect(drawer.element().getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  expect(page.getByRole('button', { name: 'Send', exact: true }).element().getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  await page.getByRole('combobox', { name: 'Message', exact: true }).fill('Plan my day');
  await expect.element(page.getByRole('combobox', { name: 'Message', exact: true })).toHaveTextContent('Plan my day');
  await page.screenshot({ path: `../../../../test-results/unified-today-${height}.png` });
  await page.getByRole('button', { name: 'Close conversation' }).click();
  await expect.element(calendar).toBeVisible();
  expect(requests.some((request) => request.path.includes('/today/launchpad'))).toBe(false);
});


it('keeps one calendar and the daily report usable at phone width', async () => {
  await page.viewport(390, 844);
  renderDailyFixture();
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  expect(calendar.all()).toHaveLength(1);
  expect(page.getByRole('button', { name: 'Track actions', exact: true }).all()).toHaveLength(1);
  const headers = [...document.querySelectorAll('[data-nc-mobile-header]')].filter((header) => header.getBoundingClientRect().width > 0);
  expect(headers).toHaveLength(1);
  await page.getByRole('button', { name: 'Track actions', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Conversations', exact: true }).click();
  await expect.element(page.getByRole('heading', { name: 'Conversations', exact: true })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
});


it.each([1000, 768, 600])('keeps the composer inside its conversation card with a long transcript at height %s', async (height) => {
  await page.viewport(1440, height);
  const history = Array.from({ length: 30 }, (_, index) => ({
    id: index + 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
    turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
    params: JSON.stringify({ item: { text: `Reply ${index + 1}.\n\n${'A detailed progress update with enough content to need scrolling. '.repeat(12)}` } }),
    created_at_ms: index + 1,
  }));
  renderDailyFixture({ reply: (request) => request.path.includes('/harness/items')
    ? { status: 200, statusText: 'OK', body: history } : undefined });
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
  const drawer = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(drawer).toBeVisible();
  await drawer.getByText('Reply 30.', { exact: false }).findElement();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  const send = page.getByRole('button', { name: 'Send', exact: true });
  await expect.element(field).toBeVisible();
  const bounds = () => {
    const stack = drawer.element().closest('[data-nc-drawer-stack]')!;
    const clip = stack.closest('main')!;
    return { drawer: drawer.element().getBoundingClientRect(), clip: clip.getBoundingClientRect(),
      field: field.element().getBoundingClientRect(), send: send.element().getBoundingClientRect() };
  };
  await expect.poll(() => bounds().send.bottom <= bounds().clip.bottom).toBe(true);
  const boxes = bounds();
  expect(boxes.field.top).toBeGreaterThanOrEqual(boxes.drawer.top);
  expect(boxes.field.bottom).toBeLessThanOrEqual(boxes.drawer.bottom);
  expect(boxes.send.bottom).toBeLessThanOrEqual(Math.min(boxes.drawer.bottom, boxes.clip.bottom, height));
  const scroll = drawer.element().querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  expect(scroll.clientHeight).toBeGreaterThan(0);
  expect(scroll.scrollHeight).toBeGreaterThan(scroll.clientHeight);
  const draft = Array.from({ length: 12 }, (_, index) => `Draft line ${index + 1}`).join('\n');
  await field.fill(draft);
  await expect.element(field).toHaveTextContent('Draft line 12');
  expect(field.element().scrollHeight).toBeGreaterThan(field.element().clientHeight);
  await expect.poll(() => bounds().send.bottom <= bounds().clip.bottom).toBe(true);
  expect(bounds().send.bottom).toBeLessThanOrEqual(Math.min(bounds().drawer.bottom, height));
  await page.screenshot({ path: `../../../../test-results/today-long-conversation-${height}.png` });
});


it.each([1000, 768, 600])('keeps both conversation composers reachable with a side draft at height %s', async (height) => {
  await page.viewport(1440, height);
  renderDailyFixture({ reply: (request) => request.path === '/api/version' ? { status: 200, statusText: 'OK', body: {
    areaCreateIdempotency: true, conversationCreateModel: true, conversationSide: true, kernelVersion: '0.1.0',
    apiVersion: '23', syncEventVersion: 26, mcpProtocolVersion: '2024-11-05', pluginMcpProtocolVersion: '2025-11-25',
    webCompatVersion: 44, minWebCompatVersion: 44, supervisorControlVersion: 1, buildSha: 'fixture',
    dbInstanceId: 'fixture-instance', databaseId: 'fixture-database', nowMs: Date.parse('2026-10-04T09:00:00+08:00'),
  } } : undefined });
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  const main = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await main.getByRole('combobox', { name: 'Message', exact: true }).fill('/side');
  await userEvent.keyboard('{Enter}');
  const side = page.getByRole('region', { name: 'Side conversation · Codex' });
  await expect.element(side).toBeVisible();
  const stack = main.element().closest('[data-nc-drawer-stack]')!;
  const clip = stack.closest('main')!;
  for (const pane of [main, side]) {
    const field = pane.getByRole('combobox', { name: 'Message', exact: true });
    const send = pane.getByRole('button', { name: 'Send', exact: true });
    await expect.element(field).toBeVisible();
    await expect.poll(() => send.element().getBoundingClientRect().bottom <= clip.getBoundingClientRect().bottom).toBe(true);
    expect(field.element().getBoundingClientRect().top).toBeGreaterThanOrEqual(pane.element().getBoundingClientRect().top);
    expect(send.element().getBoundingClientRect().bottom).toBeLessThanOrEqual(Math.min(pane.element().getBoundingClientRect().bottom, height));
  }
  await side.getByRole('combobox', { name: 'Message', exact: true }).fill('A reachable side draft.');
  await expect.element(side.getByRole('combobox', { name: 'Message', exact: true })).toHaveTextContent('A reachable side draft.');
});


it.each([768, 600])('keeps both inputs inside their cards when the parent footer grows at height %s', async (height) => {
  await page.viewport(1440, height);
  let wedged = false;
  const fixture = renderDailyFixture({ reply: (request) => {
    if (request.path === '/api/version') return { status: 200, statusText: 'OK', body: {
      areaCreateIdempotency: true, conversationCreateModel: true, conversationSide: true, kernelVersion: '0.1.0',
      apiVersion: '23', syncEventVersion: 26, mcpProtocolVersion: '2024-11-05', pluginMcpProtocolVersion: '2025-11-25',
      webCompatVersion: 44, minWebCompatVersion: 44, supervisorControlVersion: 1, buildSha: 'fixture',
      dbInstanceId: 'fixture-instance', databaseId: 'fixture-database', nowMs: Date.parse('2026-10-04T09:00:00+08:00'),
    } };
    if (request.path.endsWith('/planner/run')) return { status: 200, statusText: 'OK', body: {
      card_id: 'daily-planner', worker_session_id: 'runtime', phase: wedged ? 'wedged' : 'idle', model: null,
      reasoning_effort: null, blocked_reason: wedged ? 'The stop request timed out before the model confirmed that this turn had stopped.' : null,
      pending_queue: [], running_turn: null, final_reply: null,
    } };
    return undefined;
  } });
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  const main = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await main.getByRole('combobox', { name: 'Message', exact: true }).fill('/side');
  await userEvent.keyboard('{Enter}');
  const side = page.getByRole('region', { name: 'Side conversation · Codex' });
  await expect.element(side).toBeVisible();
  await main.getByRole('combobox', { name: 'Message', exact: true }).fill(Array.from({ length: 12 }, (_, index) => `Draft ${index + 1}`).join('\n'));
  await side.getByRole('combobox', { name: 'Message', exact: true }).fill(Array.from({ length: 12 }, (_, index) => `Side draft ${index + 1}`).join('\n'));
  wedged = true;
  await fixture.client.invalidateQueries({ queryKey: ['planner-run'] });
  await expect.element(main.getByText(/^This conversation’s session is stuck\./)).toBeVisible();
  for (const pane of [main, side]) {
    const field = pane.getByRole('combobox', { name: 'Message', exact: true });
    const send = pane.getByRole('button', { name: 'Send', exact: true });
    await expect.element(field).toBeVisible();
    await expect.poll(() => send.element().getBoundingClientRect().bottom <= pane.element().getBoundingClientRect().bottom).toBe(true);
    expect(field.element().getBoundingClientRect().top).toBeGreaterThanOrEqual(pane.element().getBoundingClientRect().top);
    expect(send.element().getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  }
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  expect(calendar.element().getBoundingClientRect().height).toBeGreaterThan(0);
  await calendar.getByRole('button', { name: 'Previous month' }).click();
  await expect.element(calendar.getByRole('heading', { name: 'September 2026' })).toBeVisible();
});
