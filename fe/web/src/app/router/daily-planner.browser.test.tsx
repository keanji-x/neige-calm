import { historicalConversationRun, historicalConversationPage, conversationCardId, humanPresentation, nativePresentation, presentationPage } from './conversation-read-data.ts';
import { withFontRoleProbe } from '../../../../tools/styles/font-role-fixture.ts';
import '../../styles/entry.css';
import { act, cleanup, fireEvent } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { transcript } from '../../../../core/events/query-keys.ts';
import { renderDailyFixture, renderWorkingDailySideFixture } from './daily-planner-fixture.tsx';

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
  const { requests } = renderDailyFixture({ initial: '/?day=2026-10-04' });
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
  renderDailyFixture({ initial: '/?day=2026-10-04' });
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  const surface = document.querySelector('[data-nc-desktop-panel]')!;
  expect(surface.contains(calendar.element())).toBe(true);
  const card = surface.firstElementChild!;
  expect(card.contains(calendar.element())).toBe(true);
  expect(card.contains(page.getByRole('heading', { name: 'Conversations', exact: true }).element())).toBe(true);
  const report = page.getByText('Prioritize the release.', { exact: false }).element();
  expect(calendar.element().getBoundingClientRect().left).toBeGreaterThan(report.getBoundingClientRect().right);
  await calendar.getByRole('link', { name: /October 3, 2026/ }).click();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await page.screenshot({ path: `../../../../test-results/today-sidebar-${height}.png` });
  await page.getByRole('button', { name: '讨论这项工作', exact: true }).click();
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
  renderDailyFixture({ initial: '/?day=2026-10-04', reply: (request) => request.path.startsWith('/api/calendar/tasks?')
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
  renderDailyFixture({ initial: '/?day=2026-10-04' });
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  document.documentElement.dataset.theme = theme;
  try {
    const monthHeading = calendar.getByRole('heading', { name: 'October 2026' });
    await expect.element(monthHeading).toBeVisible();
    const month = getComputedStyle(monthHeading.element());
    const date = getComputedStyle(page.getByRole('heading', { name: 'Sun, Oct 4' }).element());
    expect(month.fontFamily).toBe(date.fontFamily);
    expect(month.fontSize).toBe('14px');
    expect(date.fontSize).toBe(month.fontSize);
    expect(date.lineHeight).toBe('20px');
    await expect.element(page.getByText('这一天没有安排任务')).toBeVisible();
    const empty = getComputedStyle(page.getByText('这一天没有安排任务').element());
    const reference = document.createElement('span');
    reference.style.cssText = 'font:var(--type-action);color:var(--text)';
    document.body.append(reference);
    try {
      const metadata = getComputedStyle(reference);
      expect(empty.fontFamily).toBe(metadata.fontFamily);
      expect(empty.fontSize).toBe(metadata.fontSize);
      expect(empty.lineHeight).toBe('20px');
      expect(empty.color).toBe(metadata.color);
    } finally { reference.remove(); }
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
  renderDailyFixture({ initial: '/?day=2026-10-04' });
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
  renderDailyFixture({ initial: '/?day=2026-10-04', reply: (request) => request.path.includes('/harness/items')
    ? { status: 200, statusText: 'OK', body: historicalConversationPage(conversationCardId(request.path), history, null) } : undefined });
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await page.getByRole('button', { name: '讨论这项工作', exact: true }).click();
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


it.each([['/?day=2026-10-04', 1000], ['/?day=2026-10-04', 768], ['/?day=2026-10-04', 600], ['/', 600]] as const)('keeps both conversation composers reachable with a side draft on %s at height %s', async (initial, height) => {
  await page.viewport(1440, height);
  const fixture = renderDailyFixture({ initial, reply: (request) => request.path === '/api/version' ? { status: 200, statusText: 'OK', body: {
    areaCreateIdempotency: true, conversationCreateModel: true, conversationSide: true, kernelVersion: '0.1.0',
    apiVersion: '23', syncEventVersion: 26, mcpProtocolVersion: '2024-11-05', pluginMcpProtocolVersion: '2025-11-25',
    webCompatVersion: 44, minWebCompatVersion: 44, supervisorControlVersion: 1, buildSha: 'fixture',
    dbInstanceId: 'fixture-instance', databaseId: 'fixture-database',
  } } : undefined });
  if (initial !== '/') await page.getByText('Prioritize the release.', { exact: false }).findElement();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  if (initial !== '/') await page.getByRole('button', { name: '讨论这项工作', exact: true }).click();
  const main = initial === '/' ? page.getByRole('region', { name: 'Today Assistant', exact: true }) : page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(main.getByText('聊聊你想做什么', { exact: true })).toBeVisible();
  await main.getByRole('combobox', { name: 'Message', exact: true }).fill('/side');
  await userEvent.keyboard('{Enter}');
  const side = page.getByRole('region', { name: 'Side conversation · Codex' });
  await expect.poll(() => main.getByRole('combobox', { name: 'Message', exact: true }).element().textContent).toBe('/side ');
  await expect.element(side).not.toBeInTheDocument();
  expect(fixture.requests.filter(request => ['POST', 'PATCH', 'PUT', 'DELETE'].includes(request.method))).toHaveLength(0);
  await userEvent.keyboard('{Enter}');
  await expect.element(side).toBeVisible();
  const stack = side.element().closest('[data-nc-drawer-stack]')!;
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
  await side.getByRole('button', { name: 'Close side conversation', exact: true }).click();
  await expect.element(main.getByRole('combobox', { name: 'Message', exact: true })).toBeVisible();
});


it.each([768, 600])('keeps both inputs inside their cards when the parent footer grows at height %s', async (height) => {
  await page.viewport(1440, height);
  let wedged = false;
  const fixture = renderDailyFixture({ initial: '/?day=2026-10-04', reply: (request) => {
    if (request.path === '/api/version') return { status: 200, statusText: 'OK', body: {
      areaCreateIdempotency: true, conversationCreateModel: true, conversationSide: true, kernelVersion: '0.1.0',
      apiVersion: '23', syncEventVersion: 26, mcpProtocolVersion: '2024-11-05', pluginMcpProtocolVersion: '2025-11-25',
      webCompatVersion: 44, minWebCompatVersion: 44, supervisorControlVersion: 1, buildSha: 'fixture',
      dbInstanceId: 'fixture-instance', databaseId: 'fixture-database',
    } };
    if (request.path.includes('/planner/run?contract=conversation_v1')) return { status: 200, statusText: 'OK', body: historicalConversationRun({
      card_id: 'daily-planner', worker_session_id: 'runtime', phase: wedged ? 'wedged' : 'idle', model: null,
      reasoning_effort: null, permission_mode: 'never', blocked_reason: wedged ? 'The stop request timed out before the model confirmed that this turn had stopped.' : null,
      pending_queue: [], manual_compact_supported: true, recovery: wedged ? { cause: 'wedged', action: 'restart_session' } : null, compact_outcome: null, context_usage_invalidated: false, running_turn: null, final_reply: null,
    }, 'idle', true) };
    return undefined;
  } });
  await page.getByText('Prioritize the release.', { exact: false }).findElement();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await page.getByRole('button', { name: '讨论这项工作', exact: true }).click();
  const main = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(main.getByText('聊聊你想做什么', { exact: true })).toBeVisible();
  await main.getByRole('combobox', { name: 'Message', exact: true }).fill('/side');
  await userEvent.keyboard('{Enter}');
  const side = page.getByRole('region', { name: 'Side conversation · Codex' });
  await expect.poll(() => main.getByRole('combobox', { name: 'Message', exact: true }).element().textContent).toBe('/side ');
  await expect.element(side).not.toBeInTheDocument();
  expect(fixture.requests.filter(request => ['POST', 'PATCH', 'PUT', 'DELETE'].includes(request.method))).toHaveLength(0);
  await userEvent.keyboard('{Enter}');
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

it.each([1024, 1440])('aligns reading and chat headers independently of sidebar navigation sections at %ipx', async (width) => {
  await page.viewport(width, 900);
  renderDailyFixture({ initial: '/?day=2026-10-04' });
  await page.getByRole('button', { name: '讨论这项工作', exact: true }).click();
  const drawer = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(drawer).toBeVisible();
  const readingHeader = document.querySelector('[data-nc-track-page] > header')!;
  const chatHeader = drawer.element().querySelector(':scope > header')!;
  // The drawer enters through a spring; compare settled content headers.
  await expect.poll(() => Math.abs(chatHeader.getBoundingClientRect().bottom - readingHeader.getBoundingClientRect().bottom)).toBeLessThanOrEqual(1);
  const today = page.getByRole('button', { name: 'Go to Today', exact: true }).element().lastElementChild!;
  const readingTitle = page.getByRole('button', { name: 'Rename track', exact: true }).element();
  const chatTitle = chatHeader.querySelector('h2')!;
  const center = (element: Element) => {
    const rect = element.getBoundingClientRect();
    return rect.top + rect.height / 2;
  };
  expect(Math.abs(center(today) - center(readingHeader))).toBeLessThanOrEqual(1);
  expect(Math.abs(center(readingTitle) - center(readingHeader))).toBeLessThanOrEqual(1);
  await expect.poll(() => Math.abs(center(chatTitle) - center(chatHeader))).toBeLessThanOrEqual(1);
  withFontRoleProbe('--type-heading', readingHeader as HTMLElement, expected => {
    expect(getComputedStyle(readingTitle)).toMatchObject(expected);
  });
  withFontRoleProbe('--type-reading', chatHeader as HTMLElement, expected => {
    expect(getComputedStyle(chatTitle)).toMatchObject(expected);
  });
  await expect.poll(() => {
    const box = drawer.element().getBoundingClientRect();
    return Math.abs(box.top - (window.innerHeight - box.bottom));
  }).toBeLessThanOrEqual(1);
  expect(page.getByRole('button', { name: '讨论这项工作', exact: true }).query()).toBeNull();
});

it('keeps conversations ahead of the Today calendar without changing the calendar owner', async () => {
  await page.viewport(1440, 1000);
  renderDailyFixture({ initial: '/?day=2026-10-04' });
  await expect.element(page.getByRole('region', { name: 'Calendar tasks' })).toBeVisible();
  const root = document.querySelector<HTMLElement>('[data-nc-desktop-panel]')!;
  const conversations = root.querySelector<HTMLElement>('section[aria-label="Conversations"]')!;
  const calendar = page.getByRole('heading', { name: 'Calendar', exact: true }).element();
  expect(conversations.getBoundingClientRect().top).toBeLessThan(calendar.getBoundingClientRect().top);
  expect(calendar.getBoundingClientRect().left).toBe(conversations.querySelector('h2')!.getBoundingClientRect().left);
});

it('uses the shared daily empty report for the heading-only skeleton', async () => {
  await page.viewport(1440, 900);
  renderDailyFixture({ initial: '/?day=2026-10-04', reportBody: '# 概要\n\n# 今日计划' });
  await expect.element(page.getByRole('heading', { name: '今天还没有记录' })).toBeVisible();
  expect(page.getByRole('button', { name: '开始对话', exact: true }).elements()).toHaveLength(0);
});

it('shows observed SQLite writes and deferred workspaces to the owner', async () => {
  const { requests } = renderDailyFixture({
    identity: { userId: 'local-owner', displayName: 'Owner', role: 'member', sessionId: 'supervisor' },
    reply: request => request.path === '/api/today/supervisor' ? {
      status: 200, statusText: 'OK', body: {
        observed_at_ms: 1, kernel_version: '1.2.3', build_sha: 'abc123',
        settled_worker_cleanup_candidates: 0, write_bucket_ms: 300000, write_bucket_count: 6, database_writes: [{ started_at_ms: 0, transactions: 5, failures: 1, retries: 2, mean_wait_ms: 3.5 }], scheduler_boot_sweep_completed: false, retired_workspaces: { count: 2, oldest_retired_at_ms: 1 },
      },
    } : undefined,
  });
  const supervisor = page.getByRole('button', { name: 'Daily Supervisor', exact: true });
  await expect.element(supervisor).toBeVisible();
  await supervisor.click();
  await expect.element(page.getByText('abc123', { exact: true })).not.toBeVisible();
  await expect.element(page.getByText('1.2.3', { exact: true })).not.toBeVisible();
  await expect.element(page.getByText('待清理 Worker 会话', { exact: true })).toBeVisible();
  expect(requests.some(request => request.path === '/api/today/supervisor')).toBe(true);
  await expect.element(page.getByRole('button', { name: '放大查看 Daily Supervisor' })).toBeVisible();
  await expect.element(page.getByRole('img', { name: /SQLite 写事务等待.*6 intervals/ })).toBeVisible();
  await expect.element(page.getByText('调度器启动扫描尚未完成', { exact: true })).toBeVisible();
  await page.getByText('实例明细', { exact: true }).click();
  await expect.element(page.getByText('abc123', { exact: true })).toBeVisible();
  await expect.element(page.getByText('最早退役时间')).toBeVisible();
  await expect.element(page.getByText('历史巡检', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Back to Today Assistant' })).toBeVisible();
  await expect.element(page.getByRole('region', { name: 'Calendar tasks' })).toBeVisible();
});

it('does not show the Supervisor to a non-owner with the owner role label', async () => {
  const { requests } = renderDailyFixture({
    identity: { userId: 'other', displayName: 'Other', role: 'owner', sessionId: 'other' },
  });
  await expect.element(page.getByText('Report changes', { exact: true })).toBeVisible();
  expect(page.getByText('Daily Supervisor', { exact: true }).query()).toBeNull();
  expect(requests.some(request => request.path === '/api/today/supervisor')).toBe(false);
});

it('can inspect scheduler recovery before the daily Track exists', async () => {
  renderDailyFixture({
    identity: { userId: 'local-owner', displayName: 'Owner', role: 'owner', sessionId: 'preparing' },
    reply: request => {
      if (request.path === '/api/today/daily') return { status: 200, statusText: 'OK', body: null };
      if (request.path === '/api/today/supervisor') return { status: 200, statusText: 'OK', body: {
        observed_at_ms: 1, kernel_version: '1.2.3', build_sha: null,
        settled_worker_cleanup_candidates: 0, write_bucket_ms: 300000, write_bucket_count: 12, database_writes: [], scheduler_boot_sweep_completed: false, retired_workspaces: null,
      } };
    },
  });
  await expect.element(page.getByText('Preparing today’s Track…')).toBeVisible();
  await expect.element(page.getByRole('region', { name: 'Daily Supervisor' })).toBeVisible();
  await expect.element(page.getByText('调度器启动扫描尚未完成', { exact: true })).toBeVisible();
});

it.each([1024, 1440])('keeps the real Today conversation, draft and calendar in separate columns at %ipx', async width => {
  await page.viewport(width, 800);
  const { requests } = renderDailyFixture();
  const assistant = page.getByRole('region', { name: 'Today Assistant', exact: true });
  const field = assistant.getByRole('combobox', { name: 'Message', exact: true });
  await expect.element(field).toBeVisible();
  await field.fill('Help me plan today');
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await expect.element(calendar).toBeVisible();
  const send = assistant.getByRole('button', { name: 'Send', exact: true });
  expect(send.element().getBoundingClientRect().bottom).toBeLessThanOrEqual(800);
  expect(assistant.element().getBoundingClientRect().right).toBeLessThan(calendar.element().getBoundingClientRect().left);
  await page.screenshot({ path: `../../../../test-results/v10-today-assistant-${width}.png` });
  await page.getByRole('button', { name: 'Report changes', exact: true }).click();
  await expect.element(page.getByRole('heading', { name: /Report changes · 2026-10-03/ })).toBeVisible();
  await expect.element(calendar).toBeVisible();
  await page.screenshot({ path: `../../../../test-results/v10-today-changes-${width}.png` });
  await page.getByRole('button', { name: 'Back to Today Assistant', exact: true }).click();
  await expect.element(field).toHaveTextContent('Help me plan today');
  expect(requests.some(request => request.path.includes('/planner/run?contract=conversation_v1'))).toBe(true);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
});

it('scrolls real Planner history above the fixed Today composer and retains calendar selection across views', async () => {
  await page.viewport(1440, 600);
  const history = Array.from({ length: 20 }, (_, index) => ({
    id: index + 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
    turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
    params: JSON.stringify({ item: { text: `Daily reply ${index + 1}.\n\n${'Plan the release using the current evidence. '.repeat(20)}` } }),
    created_at_ms: index + 1,
  }));
  renderDailyFixture({ reply: request => request.path.includes('/harness/items')
    ? { status: 200, statusText: 'OK', body: historicalConversationPage(conversationCardId(request.path), history, null) } : undefined });
  const assistant = page.getByRole('region', { name: 'Today Assistant', exact: true });
  await assistant.getByText('Daily reply 20.', { exact: false }).findElement();
  const scroll = assistant.element().querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  expect(scroll.scrollHeight).toBeGreaterThan(scroll.clientHeight);
  const field = assistant.getByRole('combobox', { name: 'Message', exact: true });
  await field.fill('A preserved draft');
  const calendar = page.getByRole('region', { name: 'Calendar tasks' });
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await calendar.getByRole('button', { name: 'Previous month' }).click();
  await expect.element(calendar.getByRole('heading', { name: 'September 2026' })).toBeVisible();
  await page.getByRole('button', { name: 'Report changes', exact: true }).click();
  await page.getByRole('button', { name: 'Back to Today Assistant', exact: true }).click();
  await expect.element(field).toHaveTextContent('A preserved draft');
  await expect.element(calendar.getByRole('heading', { name: 'September 2026' })).toBeVisible();
  expect(assistant.getByRole('button', { name: 'Send', exact: true }).element().getBoundingClientRect().bottom).toBeLessThanOrEqual(600);
  await page.screenshot({ path: '../../../../test-results/v10-today-history.png' });
});


it.each([false, true])('navigates loaded Today exchanges and restores the parked reading position (selected: %s)', async selected => {
  await page.viewport(1440, 700);
  const items = Array.from({ length: 8 }, (_, index) => [
    humanPresentation('daily-planner', index * 2 + 1, `question-${index}`, { user_id: 'reader', display_name: 'Reader' }, `Today question ${index + 1}`, index * 2 + 1),
    nativePresentation('daily-planner', { id: index * 2 + 2, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
      turn_id: `turn-${index}`, turn_error_text: null, item_uuid: `reply-${index}`, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { type: 'agentMessage', text: `Today reply ${index + 1}. ${'Read the release evidence carefully. '.repeat(30)}` } }), created_at_ms: index * 2 + 2 }),
  ]).flat();
  renderDailyFixture({ reply: request => request.path.includes('/harness/items') ? { status: 200, statusText: 'OK', body: presentationPage(items) } : undefined });
  const assistant = page.getByRole('region', { name: 'Today Assistant', exact: true });
  const field = assistant.getByRole('combobox', { name: 'Message', exact: true });
  await field.fill('Keep my draft');
  const editor = field.element();
  await page.getByRole('button', { name: 'Jump to exchange 2: Today question 2', exact: true }).click();
  const scroll = assistant.element().querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  const marker = assistant.getByText('Today question 2', { exact: true }).element();
  await expect.poll(() => Math.abs(marker.getBoundingClientRect().top - scroll.getBoundingClientRect().top)).toBeLessThan(10);
  const overlay = assistant.element().querySelector<HTMLElement>('[data-nc-chat-scroll-overlay]')!;
  await expect.poll(() => overlay.querySelector('[data-nc-chat-scroll-dock]') !== null).toBe(true);
  const parked = scroll.scrollTop;
  await page.getByRole('button', { name: 'Report changes', exact: true }).click();
  await page.getByRole('button', { name: 'Back to Today Assistant', exact: true }).click();
  await expect.poll(() => scroll.scrollTop).toBe(parked);
  await expect.element(field).toHaveTextContent('Keep my draft');
  expect(field.element()).toBe(editor);
  await expect.element(page.getByRole('button', { name: 'Report changes', exact: true })).toHaveFocus();
  expect(document.querySelectorAll('[role="combobox"][aria-label="Message"]')).toHaveLength(1);
  await page.screenshot({ path: '../../../../test-results/v10-fixed-today-exchanges.png' });
  if (selected) await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await page.viewport(390, 844);
  if (selected) await expect.element(page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
  await page.viewport(1440, 700);
  await expect.element(field).toBeVisible();
  await expect.poll(() => scroll.scrollTop).toBe(parked);
  await expect.element(field).toHaveTextContent('Keep my draft');
  if (selected) expect(field.element()).toBe(editor);
});

it.each([false, true])('focuses every explicit Today Reply without replacing the editor (from subview: %s)', async subview => {
  await page.viewport(1440, 800);
  renderDailyFixture({ reply: request => request.path === '/api/tracks/daily' ? { status: 200, statusText: 'OK', body: {
    track: { id: 'daily', area_id: 'daily-area', title: 'Daily', sort: 0, cwd: '/tmp', workspace: { isolation: 'managed', kind: 'managed', path: '/tmp', frozen_at: null }, pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 },
    planner_revision: 0, can_reopen: false, can_admin: false, can_close: false,
    cards: [{ id: 'daily-planner', track_id: 'daily', kind: 'codex', title: 'Daily Planner conversation', sort: 1, payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 }],
    overlays: [{ id: 'activity', plugin_id: 'kernel', entity_kind: 'track', entity_id: 'daily', kind: 'activity', updated_at: 5,
      payload: { schemaVersion: 4, working: false, attention: 'input', activity_at_ms: 5, cards: [], items: [{ source: 'ask', key: 'ask:7', text: 'Choose the release branch', at_ms: 5, ask_id: 7, questions: [{ title: 'Release branch?', options: ['main'] }], delivery: 'wake' }] } }],
  } } : undefined });
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  await field.fill('An unfinished sentence');
  await userEvent.keyboard('{Home}{ArrowRight}{ArrowRight}');
  const caret = window.getSelection()?.anchorOffset;
  const editor = field.element();
  for (let index = 0; index < 2; index++) {
    if (subview) await page.getByRole('button', { name: 'Report changes', exact: true }).click();
    await page.getByRole('button', { name: 'Answer the Planner: Choose the release branch', exact: true }).click();
    await expect.element(field).toHaveFocus();
    expect(field.element()).toBe(editor);
    expect(window.getSelection()?.anchorOffset).toBe(caret);
    await expect.element(field).toHaveTextContent('An unfinished sentence');
  }
});


it('interrupts the working Today conversation from the transcript with Escape', async () => {
  await page.viewport(1440, 800);
  const { requests } = renderDailyFixture({ reply: request => {
    if (request.path.includes('/planner/run?contract=conversation_v1')) return { status: 200, statusText: 'OK', body: historicalConversationRun({ card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'turn_running', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, pending_queue: [], manual_compact_supported: true, recovery: null, compact_outcome: null, context_usage_invalidated: false, running_turn: null, final_reply: null }, 'running', true) };
    if (request.path.includes('/harness/items')) return { status: 200, statusText: 'OK', body: historicalConversationPage(conversationCardId(request.path), [{ id: 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread', turn_id: null, turn_error_text: null, item_uuid: 'reply', item_type: 'agentMessage', method: 'item/completed', params: JSON.stringify({ item: { type: 'agentMessage', text: 'Working on the [Release evidence](mailto:release@example.test).' } }), created_at_ms: 1 }], null) };
    if (request.path.endsWith('/planner/interrupt')) return { status: 200, statusText: 'OK', body: { card_id: 'daily-planner', worker_session_id: 'runtime', stopped: true } };
    return undefined;
  } });
  const assistant = page.getByRole('region', { name: 'Today Assistant', exact: true });
  await expect.element(assistant.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
  // The transcript, rather than its portalled footer, must own interruption too.
  const evidence = await assistant.getByRole('link', { name: 'Release evidence', exact: true }).findElement();
  evidence.focus();
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => requests.filter(request => request.path.endsWith('/planner/interrupt')).length).toBe(1);
  await expect.element(assistant).toBeVisible();
});

it('honors inventory and New conversation focus when leaving a Today subview', async () => {
  await page.viewport(1440, 800);
  renderDailyFixture({ extraPlanner: true });
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  await field.fill('Planner draft');
  const editor = field.element();
  await page.getByRole('button', { name: 'Report changes', exact: true }).click();
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(field).toHaveFocus();
  expect(field.element()).toBe(editor);
  await expect.element(field).toHaveTextContent('Planner draft');
  await page.getByRole('button', { name: 'Report changes', exact: true }).click();
  await page.getByRole('button', { name: 'New conversation', exact: true }).click();
  await expect.element(field).toHaveFocus();
  await expect.element(field).toHaveTextContent('');
  await field.fill('New conversation draft');
  await page.getByRole('button', { name: 'Report changes', exact: true }).click();
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(field).toHaveFocus();
  await expect.element(field).toHaveTextContent('Planner draft');
});


it.each([['Report changes', 'transcript'], ['Report changes', 'composer'], ['Daily Supervisor', 'transcript'], ['Daily Supervisor', 'composer']])('keeps an adopted working Today side interactive in %s from %s', async (view, input) => {
  await page.viewport(1440, 900);
  const { requests, rows } = renderWorkingDailySideFixture();
  const main = page.getByRole('region', { name: 'Today Assistant', exact: true });
  const mainField = main.getByRole('combobox', { name: 'Message', exact: true });
  await expect.element(main.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
  await expect.element(main.getByRole('link', { name: 'Side evidence', exact: true })).toBeVisible();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await mainField.fill('/side Explore independently');
  await userEvent.keyboard('{Enter}');
  const side = page.getByRole('region', { name: 'Side conversation · Codex', exact: true });
  await expect.element(side.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
  const sideId = rows[0].id;
  expect(requests.find(request => request.method === 'POST' && request.path.endsWith('/conversations'))?.body)
    .toMatchObject({ text: 'Explore independently', side: { source_card_id: 'daily-planner' } });
  const sideField = side.getByRole('combobox', { name: 'Message', exact: true });
  await sideField.fill('Side draft survives');
  await mainField.fill('Main draft survives');
  const mainElement = main.element();
  await page.getByRole('button', { name: view, exact: true }).click();
  await expect.element(mainElement).not.toBeVisible();
  await expect.element(side).toBeVisible();
  const evidence = await side.getByRole('link', { name: 'Side evidence', exact: true }).findElement();
  if (input === 'transcript') evidence.focus();
  else await sideField.click();
  await expect.element(input === 'transcript' ? evidence : sideField).toHaveFocus();
  if (input === 'transcript') await page.screenshot({ path: `../../../../test-results/today-side-${view === 'Report changes' ? 'changes' : 'supervisor'}.png` });
  await userEvent.keyboard('{Escape}');
  // One side request, no main request: the two working controllers must stay isolated.
  await expect.poll(() => requests.filter(request => request.path.endsWith('/planner/interrupt')).map(request => request.path))
    .toEqual([`/api/cards/${sideId}/planner/interrupt`]);
  await expect.element(side).toBeVisible();
  await expect.element(side.getByRole('button', { name: 'Stop', exact: true })).not.toBeInTheDocument();
  await page.getByRole('button', { name: 'Back to Today Assistant', exact: true }).click();
  await expect.element(mainField).toHaveTextContent('Main draft survives');
  await expect.element(sideField).toHaveTextContent('Side draft survives');
  await side.getByRole('button', { name: 'Close side conversation', exact: true }).click();
  await expect.element(page.getByRole('radio', { name: 'Month', exact: true })).toBeChecked();
});


it.each(['terminal', 'compact'])('isolates Today side visible effects while hidden by %s and restores its reading and drafts', async hiddenBy => {
  await page.viewport(1440, 900);
  const fixture = renderWorkingDailySideFixture({ paginated: true });
  const main = page.getByRole('region', { name: 'Today Assistant', exact: true });
  const mainField = main.getByRole('combobox', { name: 'Message', exact: true });
  await expect.element(main.getByText('Paragraph 34.', { exact: false })).toBeVisible();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  // Explicit selection preserves the parent identity while compact owns the main Sheet.
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await mainField.fill('/side Explore independently');
  await userEvent.keyboard('{Enter}');
  const side = page.getByRole('region', { name: 'Side conversation · Codex', exact: true });
  await expect.element(side.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
  const sideId = fixture.rows[0].id;
  const sideField = side.getByRole('combobox', { name: 'Message', exact: true });
  await mainField.fill('Main draft survives');
  await page.getByRole('button', { name: 'Report changes', exact: true }).click();

  const receiptWrites = () => fixture.receipts.requests.filter(request => request.method === 'POST' && request.path.includes('/conversation/'));
  const receiptStart = receiptWrites().length;
  act(() => { document.dispatchEvent(new Event('visibilitychange')); });
  await expect.poll(() => receiptWrites().slice(receiptStart).map(request => request.path))
    .toEqual([`/api/read-receipts/conversation/${sideId}/read`]);
  // Editing the real queue asks this side controller to focus its existing portalled editor.
  await sideField.fill('');
  await side.getByRole('button', { name: 'Edit queued message', exact: true }).click();
  await expect.element(sideField).toHaveFocus();
  await expect.element(sideField).toHaveTextContent('Explore independently');
  await side.getByRole('button', { name: 'Cancel edit', exact: true }).click();
  await sideField.fill('Side draft survives');
  const editor = sideField.element();
  const evidence = side.getByRole('link', { name: 'Side evidence', exact: true }).element();
  evidence.focus();
  await userEvent.keyboard('{Home}');
  const earlierReads = () => fixture.requests.filter(request => request.path.includes('/harness/items') && request.path.includes('&cursor='));
  await expect.poll(() => earlierReads().map(request => conversationCardId(request.path))).toEqual([sideId]);

  // A fresh server window gives the hidden branch an unspent cursor to reject.
  fixture.advanceHistory();
  await act(async () => { await fixture.client.invalidateQueries({ queryKey: transcript(sideId) }); });
  const sideElement = side.element();
  const scroller = sideElement.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  fireEvent.wheel(scroller, { deltaY: 40 });
  scroller.scrollTop = 350;
  fireEvent.scroll(scroller);
  await expect.poll(() => scroller.scrollTop).toBe(350);
  const place = scroller.scrollTop;
  const beforeHidden = receiptWrites().length;
  const readsBeforeHidden = earlierReads().length;
  if (hiddenBy === 'terminal') {
    await page.getByRole('button', { name: 'Terminal', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Hide terminal', exact: true })).toBeVisible();
    await expect.element(sideElement).not.toBeVisible();
    expect(sideElement.closest('[inert]')).not.toBeNull();
  } else {
    await page.viewport(390, 844);
    const companion = document.querySelector<HTMLElement>('[aria-label="Track workbench"]')!;
    expect(companion.hidden).toBe(true);
    expect(companion.inert).toBe(true);
  }
  act(() => {
    document.dispatchEvent(new Event('visibilitychange'));
    fireEvent.keyDown(editor, { key: 'Escape' });
    fireEvent.wheel(scroller, { deltaY: -40 });
  });
  await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  expect(receiptWrites().slice(beforeHidden).filter(request => request.path.includes(sideId))).toEqual([]);
  expect(earlierReads()).toHaveLength(readsBeforeHidden);
  expect(fixture.requests.filter(request => request.path.endsWith('/planner/interrupt'))).toEqual([]);

  if (hiddenBy === 'terminal') await page.getByRole('button', { name: 'Discussion', exact: true }).click();
  else await page.viewport(1440, 900);
  await expect.element(side).toBeVisible();
  await expect.element(sideField).toHaveTextContent('Side draft survives');
  await expect.poll(() => side.element().querySelector<HTMLElement>('[data-nc-drawer-scroll]')!.scrollTop).toBe(place);
  const resumedReceipts = receiptWrites().length;
  act(() => { document.dispatchEvent(new Event('visibilitychange')); });
  await expect.poll(() => receiptWrites().slice(resumedReceipts).some(request => request.path.includes(sideId))).toBe(true);
  // Request older history from the first loaded link, at the top of the reader.
  side.getByRole('link', { name: 'Side evidence', exact: true }).first().element().focus();
  await userEvent.keyboard('{Home}');
  await expect.poll(() => earlierReads().length).toBe(readsBeforeHidden + 1);
  expect(conversationCardId(earlierReads().at(-1)!.path)).toBe(sideId);
  await sideField.click();
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => fixture.requests.filter(request => request.path.endsWith('/planner/interrupt')).map(request => request.path))
    .toEqual([`/api/cards/${sideId}/planner/interrupt`]);
  await page.getByRole('button', { name: 'Back to Today Assistant', exact: true }).click();
  await expect.element(mainField).toHaveTextContent('Main draft survives');
  await side.getByRole('button', { name: 'Close side conversation', exact: true }).click();
  await expect.element(page.getByRole('region', { name: 'Calendar tasks', exact: true })).toBeVisible();
  if (hiddenBy === 'terminal') await expect.element(page.getByRole('radio', { name: 'Month', exact: true })).toBeChecked();
  await page.screenshot({ path: `../../../../test-results/today-side-${hiddenBy}-restored.png` });
});
