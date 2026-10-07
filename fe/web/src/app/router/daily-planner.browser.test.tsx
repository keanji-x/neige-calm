import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it('gives the daily mobile page one header and opens its scoped history', async () => {
  await page.viewport(390, 844);
  renderDailyFixture({ emptyWorkspace: true });
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  expect(page.getByRole('button', { name: 'Track actions', exact: true }).query()).toBeNull();
  const headers = [...document.querySelectorAll('[data-nc-mobile-header]')].filter(header => header.getBoundingClientRect().width > 0);
  expect(headers).toHaveLength(1);
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await page.getByRole('button', { name: '2026-10-04 Daily Planner conversation', exact: true }).click();
  await expect.element(page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
});

it.each([320, 390])('keeps mobile navigation labels and pointer targets usable at %ipx', async (width) => {
  await page.viewport(width, 844);
  renderDailyFixture({ initial: '/track/project' });
  await page.getByRole('button', { name: 'Switch track, Project evidence', exact: true }).click();
  const layer = page.getByRole('dialog', { name: 'Tracks and settings', exact: true }).element();
  await expect.poll(() => layer.getAnimations({ subtree: true }).filter(animation => animation.effect?.getTiming().iterations !== Infinity).every(animation => animation.playState !== 'running')).toBe(true);
  const bounds = layer.getBoundingClientRect();
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(width);
  const item = page.getByRole('button', { name: 'Project evidence', exact: true }).element();
  const text = item.querySelector('span:nth-child(2)')!;
  const box = text.getBoundingClientRect();
  expect(item.contains(document.elementFromPoint((box.left + box.right) / 2, (box.top + box.bottom) / 2))).toBe(true);
  await page.getByRole('button', { name: 'Back to Report', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Open workspace', exact: true })).toBeVisible();
});

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


it.each([1000, 768, 600])('shares one right sidebar card between calendar and inventory at height %s', async (height) => {
  await page.viewport(1440, height);
  renderDailyFixture();
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  const surface = document.querySelector('[data-nc-desktop-panel]')!;
  expect(surface.contains(calendar.element())).toBe(true);
  const card = surface.firstElementChild!;
  expect(calendar.element().parentElement?.parentElement?.parentElement).toBe(card);
  expect(card.contains(page.getByRole('heading', { name: 'Conversations', exact: true }).element())).toBe(true);
  const report = page.getByText('Prioritize the release.', { exact: false }).element();
  expect(calendar.element().getBoundingClientRect().left).toBeGreaterThan(report.getBoundingClientRect().right);
  await calendar.getByRole('link', { name: /October 3, 2026/ }).click();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await page.screenshot({ path: `../../../../test-results/today-sidebar-${height}.png` });
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
  const drawer = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(drawer).toBeVisible();
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  await field.fill('Plan my day');
  await expect.element(field).toHaveTextContent('Plan my day');
  expect(page.getByRole('button', { name: 'Send', exact: true }).element().getBoundingClientRect().bottom).toBeLessThanOrEqual(height);
  await page.screenshot({ path: `../../../../test-results/today-sidebar-conversation-${height}.png` });
  expect(getComputedStyle(card).visibility).toBe('hidden');
  await page.getByRole('button', { name: 'Close conversation' }).click();
  await expect.element(calendar).toBeVisible();
  await expect.element(page.getByRole('heading', { name: 'Sat, Oct 3' })).toBeVisible();
  await expect.element(page.getByRole('radio', { name: 'Month', exact: true })).toBeChecked();
});


it.each([1024, 1280])('fits the calendar date badges inside the sidebar at width %s', async (width) => {
  await page.viewport(width, 768);
  const entries = Array.from({ length: 20 }, (_, index) => ({ id: `sidebar-${index}`, version: 1, cancelled: false,
    source_track_id: null, created_by: 'user', created_at: 1, updated_at: 1, occurrences: [],
    task: { title: `Commitment ${index}`, description: '', schedule: { kind: 'all_day', date: '2026-09-28' } } }));
  renderDailyFixture({ reply: (request) => request.path.startsWith('/api/calendar/tasks?')
    ? { status: 200, statusText: 'OK', body: request.path.includes('until=2026-10-05') ? entries : [] } : undefined });
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  await expect.element(page.getByLabelText('20 tasks', { exact: true })).toBeVisible();
  const panel = calendar.element().closest('[data-nc-panel]')!;
  expect(panel.scrollWidth).toBeLessThanOrEqual(panel.clientWidth);
  for (const header of calendar.element().querySelectorAll('[role="columnheader"]')) {
    const link = header.querySelector('[role="link"]')!;
    expect(link.getBoundingClientRect().right).toBeLessThanOrEqual(header.getBoundingClientRect().right);
    const badge = link.querySelector<HTMLElement>('[class*="dateBadge"]')!;
    expect(badge.scrollWidth).toBeLessThanOrEqual(badge.clientWidth);
  }
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  expect(panel.scrollWidth).toBeLessThanOrEqual(panel.clientWidth);
});


it.each(['light', 'dark'])('uses the sidebar text roles and surfaces in %s theme', async (theme) => {
  await page.viewport(1440, 768);
  renderDailyFixture();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  document.documentElement.dataset.theme = theme;
  try {
    const month = getComputedStyle(calendar.getByRole('heading', { name: 'October 2026' }).element());
    const date = getComputedStyle(page.getByRole('heading', { name: 'Sun, Oct 4' }).element());
    expect(month.fontFamily).toBe(date.fontFamily);
    expect(month.fontSize).toBe('14px');
    expect(date.fontSize).toBe(month.fontSize);
    expect(date.lineHeight).toBe('20px');
    await expect.element(page.getByText('No tasks for this day.')).toBeVisible();
    const empty = getComputedStyle(page.getByText('No tasks for this day.').element());
    const metadata = getComputedStyle(page.getByText('No cards yet.', { exact: true }).element());
    expect(empty.fontFamily).toBe(metadata.fontFamily);
    expect(empty.fontSize).toBe(metadata.fontSize);
    expect(empty.lineHeight).toBe('16px');
    expect(empty.color).toBe(metadata.color);
    const surface = document.querySelector('[data-nc-desktop-panel]')!.firstElementChild!;
    const cardBox = surface.getBoundingClientRect();
    const switchBox = page.getByRole('radio', { name: 'Month', exact: true }).element().getBoundingClientRect();
    expect(switchBox.top - cardBox.top).toBeGreaterThanOrEqual(8);
    expect(cardBox.right - switchBox.right).toBeGreaterThanOrEqual(16);
    expect(getComputedStyle(surface).backgroundColor).not.toBe(getComputedStyle(document.body).backgroundColor);
    await page.screenshot({ path: `../../../../test-results/today-sidebar-${theme}.png` });
  } finally {
    document.documentElement.dataset.theme = 'light';
  }
});


it('keeps one calendar and the daily report usable at phone width', async () => {
  await page.viewport(390, 844);
  renderDailyFixture();
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  expect(calendar.all()).toHaveLength(1);
  expect(page.getByRole('button', { name: 'Track actions', exact: true }).query()).toBeNull();
  const headers = [...document.querySelectorAll('[data-nc-mobile-header]')].filter((header) => header.getBoundingClientRect().width > 0);
  expect(headers).toHaveLength(1);
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await expect.element(page.getByRole('dialog', { name: '历史对话', exact: true })).toBeVisible();
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
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
  const drawer = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(drawer).toBeVisible();
  await drawer.getByText('Reply 30.', { exact: false }).findElement();
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
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
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
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await page.getByRole('button', { name: 'Planner', exact: true }).click();
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
  await page.getByRole('button', { name: 'Close side conversation' }).click();
  await page.getByRole('button', { name: 'Close conversation' }).click();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  await calendar.getByRole('button', { name: 'Previous month' }).click();
  await expect.element(calendar.getByRole('heading', { name: 'September 2026' })).toBeVisible();
});
