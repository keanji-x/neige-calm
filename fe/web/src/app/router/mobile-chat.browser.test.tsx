import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it.each([320, 390])('aligns the left circular controls and keeps chat usable at %ipx', async (width) => {
  await page.viewport(width, 844);
  const { requests } = renderDailyFixture({ emptyWorkspace: true });
  const field = await page.getByRole('textbox', { name: 'Chat message', exact: true }).findElement();
  const bar = field.closest('form')!;
  const closedBarLeft = bar.getBoundingClientRect().left;
  expect(bar.getBoundingClientRect().left).toBeGreaterThan(0);
  expect(bar.getBoundingClientRect().right).toBeLessThan(width);
  expect(getComputedStyle(bar).borderRadius).toBe('999px');
  const leading = page.getByRole('button', { name: 'Open conversation history', exact: true }).element().getBoundingClientRect();
  const title = page.getByRole('button', { name: 'Switch track, 2026-10-04', exact: true }).element().getBoundingClientRect();
  const workspace = page.getByRole('button', { name: 'Open workspace', exact: true }).element().getBoundingClientRect();
  expect(new Set([leading.height, title.height, workspace.height, bar.getBoundingClientRect().height].map(Math.round)).size).toBe(1);
  expect(workspace.left).toBe(leading.left);
  expect(bar.getBoundingClientRect().right).toBe(title.right);
  expect(width - bar.getBoundingClientRect().right).toBe(leading.left);
  expect(title.left).toBeGreaterThanOrEqual(leading.right);
  expect(title.right).toBeLessThanOrEqual(width - leading.left);
  const historyIcon = page.getByRole('button', { name: 'Open conversation history', exact: true }).element().querySelector('svg')!;
  const workspaceIcon = page.getByRole('button', { name: 'Open workspace', exact: true }).element().querySelector('svg')!;
  expect(historyIcon.getAttribute('viewBox')).toBe(workspaceIcon.getAttribute('viewBox'));
  expect(historyIcon.getAttribute('stroke-width')).toBe(workspaceIcon.getAttribute('stroke-width'));
  expect(historyIcon.getBoundingClientRect().width).toBe(workspaceIcon.getBoundingClientRect().width);
  const surface = getComputedStyle(bar);
  for (const name of ['Open conversation history', 'Open workspace']) {
    const paint = getComputedStyle(page.getByRole('button', { name, exact: true }).element());
    expect([paint.backgroundColor, paint.borderColor, paint.boxShadow, paint.backdropFilter])
      .toEqual([surface.backgroundColor, surface.borderColor, surface.boxShadow, surface.backdropFilter]);
  }
  const titlePaint = getComputedStyle(page.getByRole('button', { name: 'Switch track, 2026-10-04', exact: true }).element());
  expect(titlePaint.backgroundColor).toBe('rgba(0, 0, 0, 0)');
  expect(titlePaint.borderTopWidth).toBe('0px');
  expect(titlePaint.boxShadow).toBe('none');
  for (const name of ['Open conversation history']) {
    const button = page.getByRole('button', { name, exact: true }).element();
    expect(getComputedStyle(button).borderRadius).toBe('999px');
  }
  expect(page.getByRole('button', { name: 'Track actions', exact: true }).query()).toBeNull();
  await page.getByRole('button', { name: 'Switch track, 2026-10-04', exact: true }).click();
  await expect.element(page.getByRole('group', { name: 'Areas', exact: true })).toBeVisible();
  expect(page.getByRole('menu').query()).toBeNull();
  await page.getByRole('button', { name: 'Back to workspace', exact: true }).click();
  await page.getByRole('button', { name: 'Open workspace', exact: true }).click();
  await expect.element(page.getByRole('group', { name: 'Areas', exact: true })).toBeVisible();
  expect(page.getByRole('radio').query()).toBeNull();
  expect(page.getByRole('radio').query()).toBeNull();
  await page.getByRole('button', { name: 'Back to workspace', exact: true }).click();
  await expect.element(page.getByRole('textbox', { name: 'Chat message', exact: true })).toBeVisible();
  page.getByRole('textbox', { name: 'Chat message', exact: true }).element().focus();
  const history = await page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true }).findElement();
  const message = page.getByRole('combobox', { name: 'Message', exact: true }).element();
  expect(message.getBoundingClientRect().width).toBeGreaterThan(width - 180);
  const panel = history.querySelector<HTMLElement>('[data-nc-drawer]')!;
  const sheet = panel.closest('[data-nc-mobile-chat-panel]')!;
  expect(getComputedStyle(sheet.firstElementChild!).backgroundImage).toBe('none');
  const historyButton = page.getByRole('button', { name: 'Open conversation history', exact: true }).element();
  expect(sheet.getBoundingClientRect().top - historyButton.getBoundingClientRect().bottom).toBeGreaterThan(200);
  const sendIcon = history.querySelector('[aria-label="Send"] svg')!;
  expect(sendIcon.getBoundingClientRect().width).toBe(24);
  const bounds = panel.getBoundingClientRect();
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(width);
  expect(bounds.height).toBeGreaterThan(844 * 0.6);
  expect(bounds.height).toBeLessThan(844 * 0.75);
  await expect.poll(() => document.activeElement === message).toBe(true);
  const expandedBar = message.closest('[class*="vendorComposer"]')?.firstElementChild;
  expect(expandedBar).toBeTruthy();
  expect(Math.round(expandedBar!.getBoundingClientRect().left)).toBe(Math.round(closedBarLeft));
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await expect.poll(() => document.querySelector('dialog[open]')).toBeNull();
  await expect.element(page.getByRole('textbox', { name: 'Chat message', exact: true })).toBeVisible();
  expect(document.activeElement?.hasAttribute('data-nc-chat-dock')).toBe(true);
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});

it('keeps draft words across closing and sends the same Planner before reopening its reply history', async () => {
  await page.viewport(390, 844);
  let answered = false;
  const { requests, client } = renderDailyFixture({ emptyWorkspace: true, reply: (request) => {
    if (request.path.endsWith('/planner/input')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime' });
    if (request.path.includes('/harness/items') && answered) return ok([{
      id: 1, worker_session_id: 'runtime', card_id: 'daily-planner', track_id: 'daily', thread_id: 'thread',
      turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: 'Your plan is ready.' } }), created_at_ms: 1,
    }]);
    return undefined;
  } });
  const entry = page.getByRole('textbox', { name: 'Chat message', exact: true });
  (await entry.findElement() as HTMLElement).focus();
  await page.getByRole('combobox', { name: 'Message', exact: true }).click();
  await userEvent.keyboard('整理今天的计划{Shift>}{Enter}{/Shift}保留这行');
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await expect.poll(() => document.querySelector('dialog[open]')).toBeNull();
  await expect.element(entry).toHaveValue('整理今天的计划\n保留这行');
  await page.getByRole('button', { name: 'Send chat message', exact: true }).click();
  await expect.poll(() => requests.filter((request) => request.path.endsWith('/planner/input')).length).toBe(1);
  const sent = requests.find((request) => request.path.endsWith('/planner/input'))!;
  expect(sent.path).toBe('/api/cards/daily-planner/planner/input');
  expect(sent.body).toMatchObject({ text: '整理今天的计划\n保留这行' });
  expect(sent.headers?.['Idempotency-Key']).toBeTruthy();
  await expect.element(page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
  answered = true;
  await client.invalidateQueries();
  await expect.element(page.getByText('Your plan is ready.', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await expect.poll(() => document.querySelector('dialog[open]')).toBeNull();
  entry.element().focus();
  await expect.element(page.getByText('Your plan is ready.', { exact: true })).toBeVisible();
  expect(requests.filter((request) => request.method === 'POST' && request.path.endsWith('/conversations'))).toHaveLength(0);
});

it('shows no draft entry on desktop and never dispatches an empty message', async () => {
  await page.viewport(390, 844);
  const { requests } = renderDailyFixture({ emptyWorkspace: true });
  await expect.element(page.getByRole('button', { name: 'Send chat message', exact: true })).toBeDisabled();
  await page.viewport(1280, 800);
  await expect.poll(() => page.getByRole('textbox', { name: 'Chat message', exact: true }).query()).toBeNull();
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});

it('keeps workspace navigation as Area and Track lists with no filters', async () => {
  await page.viewport(390, 844);
  const { requests } = renderDailyFixture();
  await page.getByRole('button', { name: 'Open workspace', exact: true }).click();
  await page.getByRole('button', { name: 'Project', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Project evidence', exact: true })).toBeVisible();
  expect(page.getByRole('radio').query()).toBeNull();
  await page.getByRole('button', { name: 'Area actions', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'New track', exact: true })).toBeVisible();
  await expect.element(page.getByRole('menuitem', { name: 'Edit area Project', exact: true })).toBeVisible();
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});

it('opens the existing Track page from the title and returns directly to the report', async () => {
  await page.viewport(390, 844);
  const { requests, router } = renderDailyFixture();
  await page.getByRole('button', { name: 'Switch track, 2026-10-04', exact: true }).click();
  await expect.element(page.getByRole('group', { name: 'Tracks', exact: true })).toBeVisible();
  expect(page.getByRole('menu').query()).toBeNull();
  await expect.element(page.getByRole('button', { name: 'Project evidence', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Back to Report', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Switch track, 2026-10-04', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Switch track, 2026-10-04', exact: true }).click();
  await page.getByRole('button', { name: 'Project evidence', exact: true }).click();
  await expect.poll(() => router.state.location.pathname).toBe('/track/project');
  await page.getByRole('button', { name: 'Switch track, Project evidence', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Project evidence', exact: true })).toHaveAttribute('aria-current', 'page');
  await page.getByRole('button', { name: 'Back to Report', exact: true }).click();
  expect(router.state.location.pathname).toBe('/track/project');
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});


it.each([320, 390])('keeps a long title truncated and its switch visible at %ipx', async (width) => {
  await page.viewport(width, 844);
  const projectTitle = '手机端交互与视觉体验优化：工作区、历史对话、报告阅读与输入栏的完整设计';
  const { requests } = renderDailyFixture({ initial: '/track/project', projectTitle });
  const title = await page.getByRole('button', { name: `Switch track, ${projectTitle}`, exact: true }).findElement();
  const label = title.querySelector('span')!;
  const icon = title.querySelector('svg')!;
  expect(title.getBoundingClientRect().right).toBeLessThanOrEqual(width - 16);
  expect(label.scrollWidth).toBeGreaterThan(label.clientWidth);
  expect(label.getBoundingClientRect().right).toBeLessThanOrEqual(icon.getBoundingClientRect().left);
  expect(icon.getBoundingClientRect().width).toBe(24);
  await page.getByRole('button', { name: `Switch track, ${projectTitle}`, exact: true }).click();
  await expect.element(page.getByRole('button', { name: projectTitle, exact: true })).toBeVisible();
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});

it('keeps the fixed mobile editor above a reduced visible viewport without losing its draft', async () => {
  await page.viewport(390, 844);
  renderDailyFixture({ emptyWorkspace: true });
  await page.getByRole('textbox', { name: 'Chat message', exact: true }).click();
  const editor = await page.getByRole('combobox', { name: 'Message', exact: true }).findElement();
  await expect.poll(() => document.activeElement === editor).toBe(true);
  await page.getByRole('combobox', { name: 'Message', exact: true }).fill('Keep this draft');
  const viewport = window.visualViewport!;
  const descriptor = Object.getOwnPropertyDescriptor(viewport, 'height');
  try {
    Object.defineProperty(viewport, 'height', { configurable: true, value: 480 });
    viewport.dispatchEvent(new Event('resize'));
    await expect.poll(() => editor.getBoundingClientRect().bottom).toBeLessThanOrEqual(480);
    const panel = document.querySelector<HTMLElement>('[data-nc-mobile-chat-panel]')!;
    const navigation = page.getByRole('button', { name: 'Open conversation history', exact: true }).element();
    await expect.poll(() => panel.getAnimations().every((animation) => animation.playState !== 'running')).toBe(true);
    await expect.poll(() => panel.getBoundingClientRect().top - navigation.getBoundingClientRect().bottom).toBeGreaterThanOrEqual(16);
    expect(document.activeElement).toBe(editor);
    await expect.element(page.getByRole('combobox', { name: 'Message', exact: true })).toHaveTextContent('Keep this draft');
  } finally {
    if (descriptor === undefined) Reflect.deleteProperty(viewport, 'height');
    else Object.defineProperty(viewport, 'height', descriptor);
    viewport.dispatchEvent(new Event('resize'));
  }
});

it('keeps the message viewport above a growing multiline composer', async () => {
  await page.viewport(390, 844);
  renderDailyFixture({ emptyWorkspace: true });
  await page.getByRole('textbox', { name: 'Chat message', exact: true }).click();
  const field = page.getByRole('combobox', { name: 'Message', exact: true });
  await expect.element(field).toBeEnabled();
  const footer = document.querySelector<HTMLElement>('[data-nc-chat-footer]')!;
  const initialHeight = footer.getBoundingClientRect().height;
  await field.fill('First line\nSecond line\nThird line\nFourth line\nFifth line\nSixth line');
  await expect.poll(() => footer.getBoundingClientRect().height).toBeGreaterThan(initialHeight);
  const messages = document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  await expect.poll(() => messages.getBoundingClientRect().bottom - footer.getBoundingClientRect().top).toBeLessThanOrEqual(1);
  expect(Math.round(footer.getBoundingClientRect().bottom)).toBe(844);
});

it('loads real mobile CJK weight ranges and keeps shadowed controls free of visible outlines', async () => {
  await page.viewport(390, 844);
  renderDailyFixture({ emptyWorkspace: true });
  const entry = await page.getByRole('textbox', { name: 'Chat message', exact: true }).findElement();
  const faces = await document.fonts.load('500 15px "Neige Mobile CJK"', '工作区手机对话');
  expect(faces.length).toBeGreaterThan(0);
  expect(faces.every(face => face.status === 'loaded' && face.weight === '400 700')).toBe(true);
  const surface = getComputedStyle(entry.closest('form')!);
  expect(surface.boxShadow).not.toBe('none');
  expect(surface.borderColor).toBe('rgba(0, 0, 0, 0)');
});


it.each(['/track/project', '/area/project-area/new'])('returns from mobile track creation to its workspace without writes from %s', async (initial) => {
  await page.viewport(390, 844);
  const { router, requests } = renderDailyFixture({ initial });
  if (initial === '/track/project') {
    await page.getByRole('button', { name: 'Switch track, Project evidence', exact: true }).click();
    await page.getByRole('button', { name: 'New track', exact: true }).click();
  }
  const draft = page.getByRole('combobox', { name: 'What this track should do', exact: true });
  await draft.fill('保留未提交的 Track 草稿');
  const back = page.getByRole('button', { name: 'Back to Tracks', exact: true });
  await expect.element(back).toBeVisible();
  await back.click();
  await expect.element(page.getByRole('group', { name: 'Tracks', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Project evidence', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'New track', exact: true }).click();
  await expect.element(draft).toHaveTextContent('保留未提交的 Track 草稿');
  await back.click();
  await page.getByRole('button', { name: 'Back to Areas', exact: true }).click();
  await page.getByRole('button', { name: 'Back to workspace', exact: true }).click();
  await expect.poll(() => router.state.location.pathname).toBe(initial === '/track/project' ? '/track/project' : '/');
  await expect.poll(() => document.activeElement?.getAttribute('aria-label')).toBe('Open conversation history');
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});


it('keeps Area editor actions inside the visible viewport when its keyboard opens', async () => {
  await page.viewport(390, 844);
  const { requests } = renderDailyFixture();
  await page.getByRole('button', { name: 'Open workspace', exact: true }).click();
  await page.getByRole('button', { name: 'New area', exact: true }).click();
  const dialog = await page.getByRole('dialog', { name: 'New area', exact: true }).findElement();
  const name = await page.getByRole('textbox', { name: /^Name/ }).findElement();
  await expect.poll(() => document.activeElement === name).toBe(true);
  await expect.poll(() => dialog.getAnimations().every(animation => animation.playState !== 'running')).toBe(true);
  const viewport = window.visualViewport!;
  const descriptors = ['height', 'offsetTop'].map(key => Object.getOwnPropertyDescriptor(viewport, key));
  try {
    for (const offsetTop of [0, 70]) {
      Object.defineProperty(viewport, 'height', { configurable: true, value: 480 });
      Object.defineProperty(viewport, 'offsetTop', { configurable: true, value: offsetTop });
      viewport.dispatchEvent(new Event('resize'));
      await expect.poll(() => dialog.getBoundingClientRect().bottom).toBeLessThanOrEqual(480 + offsetTop);
      expect(dialog.getBoundingClientRect().top).toBeGreaterThanOrEqual(offsetTop);
      expect(page.getByRole('button', { name: 'Cancel', exact: true }).element().getBoundingClientRect().bottom).toBeLessThanOrEqual(480 + offsetTop);
      expect(document.activeElement).toBe(name);
    }
  } finally {
    ['height', 'offsetTop'].forEach((key, index) => {
      const descriptor = descriptors[index];
      if (descriptor === undefined) Reflect.deleteProperty(viewport, key);
      else Object.defineProperty(viewport, key, descriptor);
    });
    viewport.dispatchEvent(new Event('resize'));
  }
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect.element(page.getByRole('group', { name: 'Areas', exact: true })).toBeVisible();
  expect(requests.filter(request => request.method !== 'GET')).toHaveLength(0);
});


it('interrupts the current working conversation from its portalled mobile editor without dismissing it', async () => {
  await page.viewport(390, 844);
  const { requests } = renderDailyFixture({ emptyWorkspace: true, reply: request => {
    if (request.path.endsWith('/planner/run')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', phase: 'turn_running', model: null, reasoning_effort: null, blocked_reason: null, pending_queue: [], running_turn: null, final_reply: null });
    if (request.path.endsWith('/planner/interrupt')) return ok({ card_id: 'daily-planner', worker_session_id: 'runtime', stopped: true });
    return undefined;
  } });
  await page.getByRole('textbox', { name: 'Chat message', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
  await page.getByRole('combobox', { name: 'Message', exact: true }).click();
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => requests.filter(request => request.path === '/api/cards/daily-planner/planner/interrupt')).toHaveLength(1);
  await expect.element(page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
});
