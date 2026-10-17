import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { renderDailyFixture } from '../router/daily-planner-fixture.tsx';
import { createUiPreferences } from '../providers/ui-preferences.tsx';

afterEach(cleanup);

it('explains the current contextual work card while preserving its native conversations', async () => {
  await page.viewport(1440, 900);
  const preferences = createUiPreferences(); preferences.setDatabaseScope('current-work-card');
  preferences.setGuideEnabled(true);
  for (const id of ['track-actions', 'track-report']) preferences.setGuideStepSeen(1, id, true);
  const fixture = renderDailyFixture({ initial: '/track/project', uiPreferences: preferences });
  const hint = page.getByRole('note', { name: '当前工作的对话与进展' });
  await expect.element(hint).toBeVisible();
  expect(hint.element().textContent).toContain('Completed');
  await expect.element(page.getByRole('heading', { name: 'Conversations', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'New conversation', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '跳过引导', exact: true }).click();
  await expect.element(hint).not.toBeInTheDocument();
  await expect.element(page.getByRole('button', { name: 'New conversation', exact: true })).toBeVisible();
  fixture.client.clear();
});

it('automatically enrolls a loaded empty workspace and remembers Skip', async () => {
  await page.viewport(1280, 900);
  const preferences = createUiPreferences();
  preferences.setDatabaseScope('guide-test');
  const first = renderDailyFixture({ emptyWorkspace: true, uiPreferences: preferences });
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).toBeVisible();
  expect(preferences.guideEnabled()).toBe(true);
  const add = page.getByRole('button', { name: 'New area', exact: true }).element().getBoundingClientRect();
  const pointer = page.getByRole('note', { name: '先做出一个能预览的小作品' }).element().querySelector('svg')!.getBoundingClientRect();
  expect(Math.abs(pointer.left + pointer.width / 2 - (add.left + add.width / 2))).toBeLessThanOrEqual(2);
  await page.getByRole('button', { name: '跳过引导', exact: true }).click();
  expect(preferences.guideEnabled()).toBe(false);
  first.client.clear(); cleanup();
  const second = renderDailyFixture({ emptyWorkspace: true, uiPreferences: preferences });
  await expect.element(page.getByRole('button', { name: 'New area', exact: true })).toBeVisible();
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).not.toBeInTheDocument();
  second.client.clear();
});

it('keeps existing projects ordinary and starts the relevant page guide on request', async () => {
  await page.viewport(1280, 900);
  const preferences = createUiPreferences(); preferences.setDatabaseScope('existing-project');
  const fixture = renderDailyFixture({ initial: '/track/project', uiPreferences: preferences });
  await expect.element(page.getByText('Project result', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('note', { name: '继续当前工作，还是新开一项？' })).not.toBeInTheDocument();
  expect(preferences.guideEnabled()).toBe(false);
  await page.getByRole('button', { name: 'Account menu for You', exact: true }).click();
  await page.getByRole('menuitem', { name: '入门引导', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '继续当前工作，还是新开一项？' })).toBeVisible();
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).not.toBeInTheDocument();
  await page.getByRole('button', { name: '略过这一步', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '进展和成果都汇总在这里' })).toBeVisible();
  await page.getByRole('button', { name: '上一步', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '继续当前工作，还是新开一项？' })).toBeVisible();
  fixture.client.clear();
});

it.each([{ width: 1280, height: 900 }, { width: 390, height: 844 }, { width: 320, height: 640 }])('keeps native Area fields and actions usable with the grouped guide at $width px', async ({ width, height }) => {
  await page.viewport(width, height);
  const fixture = renderDailyFixture({ emptyWorkspace: true, initialWorkspaceGuide: true, reply: request => {
    if (request.path.startsWith('/api/fs/listdir')) return { status: 200, statusText: 'OK', body: { path: '/projects', parent: '/', entries: [] } };
    if (request.path === '/api/track-templates') return { status: 200, statusText: 'OK', body: [{ id: 'small-change', title: 'Small change', description: null, tasks: [], availability: { status: 'ready' } }] };
    return undefined;
  } });
  await page.getByRole('button', { name: 'New area', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '先给练习项目取个名字' })).toBeVisible();
  const name = page.getByRole('textbox', { name: 'Name', exact: true });
  expect(document.activeElement).toBe(name.element());
  await name.fill('Keep my project');
  await expect.element(page.getByRole('button', { name: '填入这个名字' })).not.toBeInTheDocument();
  await page.getByRole('button', { name: '下一步', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '这次先做一个本地小练习' })).toBeVisible();
  await page.getByRole('button', { name: 'Default template: No template', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Small change', exact: true }).click();
  await page.getByRole('button', { name: '下一步', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '选一个可以放心练习的目录' })).toBeVisible();
  for (const label of ['Cancel', 'Create area']) {
    const button = page.getByRole('button', { name: label, exact: true }).element();
    await expect.poll(() => {
      const bounds = button.getBoundingClientRect();
      return button.contains(document.elementFromPoint(bounds.left + bounds.width / 2, bounds.top + bounds.height / 2));
    }).toBe(true);
  }
  await page.getByRole('button', { name: 'Default folder: Neige workspace', exact: true }).click();
  await expect.element(page.getByRole('dialog', { name: 'Choose a directory', exact: true })).toBeVisible();
  await expect.element(page.getByRole('note', { name: '选一个可以放心练习的目录' })).not.toBeInTheDocument();
  await page.getByRole('button', { name: 'Select this directory', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '选一个可以放心练习的目录' })).toBeVisible();
  await page.getByRole('button', { name: '跳过引导', exact: true }).click();
  await expect.element(name).toHaveValue('Keep my project');
  const folder = page.getByRole('button', { name: 'Default folder: /projects', exact: true });
  expect(document.activeElement).toBe(folder.element());
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect.element(page.getByRole('dialog', { name: 'New area', exact: true })).not.toBeInTheDocument();
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path === '/api/areas')).toHaveLength(0);
  fixture.client.clear();
});

it('does not enroll from a failed empty read or an unavailable optional API', async () => {
  await page.viewport(1280, 900);
  const preferences = createUiPreferences(); preferences.setDatabaseScope('failed-read');
  const fixture = renderDailyFixture({ uiPreferences: preferences, reply: request => request.path === '/api/areas'
    ? { status: 500, statusText: 'Error', body: { error: 'Offline' } } : undefined });
  await expect.poll(() => fixture.requests.some(request => request.path === '/api/areas')).toBe(true);
  expect(preferences.guideEnabled()).toBe(null);
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).not.toBeInTheDocument();
  fixture.client.clear(); cleanup();
  const descriptor = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'showPopover')!;
  Object.defineProperty(HTMLElement.prototype, 'showPopover', { ...descriptor, value: undefined });
  try {
    const unsupported = renderDailyFixture({ emptyWorkspace: true, initialWorkspaceGuide: true });
    await page.getByRole('button', { name: 'New area', exact: true }).click();
    await expect.element(page.getByRole('textbox', { name: 'Name', exact: true })).toBeVisible();
    await expect.element(page.getByRole('note', { name: '选一个可以放心练习的目录' })).not.toBeInTheDocument();
    unsupported.client.clear();
  } finally { cleanup(); Object.defineProperty(HTMLElement.prototype, 'showPopover', descriptor); }
});


it('waits for the stable database identity before enrolling a first workspace', async () => {
  await page.viewport(1280, 900);
  const preferences = createUiPreferences();
  const fixture = renderDailyFixture({ emptyWorkspace: true, uiPreferences: preferences });
  await expect.element(page.getByRole('button', { name: 'New area', exact: true })).toBeVisible();
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).not.toBeInTheDocument();
  preferences.setDatabaseScope('late-database');
  await expect.poll(() => preferences.guideEnabled()).toBe(true);
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).toBeVisible();
  expect(preferences.guideEnabled()).toBe(true);
  fixture.client.clear();
});

it('opens compact first-use navigation once and lets the reader restart help there', async () => {
  await page.viewport(390, 844);
  const preferences = createUiPreferences(); preferences.setDatabaseScope('compact-first');
  const fixture = renderDailyFixture({ emptyWorkspace: true, uiPreferences: preferences });
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).toBeVisible();
  await page.getByRole('button', { name: '跳过引导', exact: true }).click();
  await page.getByRole('button', { name: 'Workspace actions', exact: true }).click();
  await page.getByRole('menuitem', { name: '入门引导', exact: true }).click();
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).toBeVisible();
  fixture.client.clear();
});

it('keeps Skip effective when an explicitly guided visit confirms its database later', async () => {
  await page.viewport(1280, 900);
  const preferences = createUiPreferences();
  const fixture = renderDailyFixture({ emptyWorkspace: true, initialWorkspaceGuide: true, uiPreferences: preferences });
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).toBeVisible();
  await page.getByRole('button', { name: '跳过引导', exact: true }).click();
  preferences.setDatabaseScope('confirmed-after-skip');
  await expect.poll(() => preferences.guideEnabled()).toBe(false);
  await expect.element(page.getByRole('note', { name: '先做出一个能预览的小作品' })).not.toBeInTheDocument();
  fixture.client.clear();
});

it('continues to visible workspace controls after a report tour and reveals the actual add glyph', async () => {
  await page.viewport(1280, 900);
  const preferences = createUiPreferences(); preferences.setDatabaseScope('continue-tour'); preferences.setGuideEnabled(true);
  for (const id of ['track-actions', 'track-report', 'track-panel', 'preview-open']) preferences.setGuideStepSeen(1, id, true);
  const fixture = renderDailyFixture({ initial: '/track/project', uiPreferences: preferences });
  await expect.element(page.getByRole('note', { name: '现在给 AI 一件具体的事做' })).toBeVisible();
  const add = page.getByRole('button', { name: 'New track in Project', exact: true });
  expect(getComputedStyle(add.element()).opacity).toBe('1');
  fixture.client.clear();
});
