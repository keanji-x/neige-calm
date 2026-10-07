import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUiPreferences } from '../providers/ui-preferences.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

it('separates history from workspace navigation and hands a selection to the existing Planner', async () => {
  await page.viewport(390, 844);
  const { requests } = renderDailyFixture({ emptyWorkspace: true });
  await page.getByRole('textbox', { name: 'Chat message', exact: true }).findElement();
  expect(page.getByRole('button', { name: 'Track actions', exact: true }).query()).toBeNull();
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  const history = page.getByRole('dialog', { name: '历史对话', exact: true });
  await expect.element(history).toBeVisible();
  await expect.element(page.getByRole('searchbox', { name: '搜索对话内容（暂未开放）', exact: true })).toBeDisabled();
  await expect.element(page.getByRole('button', { name: '发起新对话', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '2026-10-04 Daily Planner conversation', exact: true }).click();
  await expect.poll(() => history.query()).toBeNull();
  await expect.element(page.getByRole('dialog', { name: 'Daily Planner conversation', exact: true })).toBeVisible();
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});

it('closes history before opening a new conversation and Settings', async () => {
  await page.viewport(390, 844);
  const { requests } = renderDailyFixture({ emptyWorkspace: true });
  await page.getByRole('textbox', { name: 'Chat message', exact: true }).findElement();
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await page.getByRole('button', { name: '发起新对话', exact: true }).click();
  await expect.element(page.getByRole('dialog', { name: 'Untitled', exact: true })).toBeVisible();
  expect(document.querySelector('dialog[open][aria-label="历史对话"]')).toBeNull();
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await page.getByRole('button', { name: '设置', exact: true }).click();
  await expect.element(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible();
  expect(document.querySelector('dialog[open][aria-label="历史对话"]')).toBeNull();
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});

it('reads only current Track conversations and excludes sibling and other workspace histories', async () => {
  await page.viewport(390, 844);
  const track = { id: 'project', area_id: 'project-area', title: 'Project evidence', sort: 0, cwd: '/tmp',
    pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const sibling = { ...track, id: 'sibling', title: 'Sibling report' };
  const foreign = { ...track, id: 'foreign', area_id: 'other-area', title: 'Other report' };
  const { requests } = renderDailyFixture({ initial: '/track/project', reply: (request) => {
    if (request.path === '/api/areas') return ok([
      { id: 'project-area', name: 'Project', kind: 'user', color: '#6574cd', sort: 0, created_at: 1, updated_at: 1 },
      { id: 'other-area', name: 'Other', kind: 'user', color: '#6574cd', sort: 1, created_at: 1, updated_at: 1 },
    ]);
    if (request.path === '/api/areas/project-area/tracks') return ok([track, sibling]);
    if (request.path === '/api/areas/other-area/tracks') return ok([foreign]);
    if (request.path === '/api/tracks/sibling') return ok({ track: sibling, can_reopen: false, can_close: true, cards: [], overlays: [] });
    if (request.path === '/api/tracks/project/conversations') return ok([
      { id: 'first-chat', trackId: 'project', title: 'Older chat', kind: 'track-assistant', state: null, updatedAt: 10, lastTurnCompletedAt: null },
    ]);
    if (request.path === '/api/tracks/sibling/conversations') return ok([
      { id: 'second-chat', trackId: 'sibling', title: 'Newest chat', kind: 'track-assistant', state: null, updatedAt: 20, lastTurnCompletedAt: null },
    ]);
    return undefined;
  } });
  await page.getByRole('textbox', { name: 'Chat message', exact: true }).findElement();
  await expect.poll(() => requests.some((request) => request.path === '/api/areas/project-area/tracks')).toBe(true);
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Older chat Project evidence', exact: true })).toBeVisible();
  expect(page.getByRole('button', { name: 'Newest chat Sibling report', exact: true }).query()).toBeNull();
  expect(requests.some((request) => request.path === '/api/tracks/sibling/conversations')).toBe(false);
  expect(requests.some((request) => request.path === '/api/tracks/foreign/conversations')).toBe(false);
  expect(requests.some((request) => request.path === '/api/tracks/foreign')).toBe(false);

});

it('keeps history failures distinct from an empty workspace and can retry', async () => {
  await page.viewport(390, 844);
  let fail = true;
  renderDailyFixture({ emptyWorkspace: true, reply: (request) => request.path === '/api/tracks/daily/conversations' && fail
    ? { status: 500, statusText: 'Error', body: { error: 'unavailable' } } : undefined });
  await page.getByRole('textbox', { name: 'Chat message', exact: true }).findElement();
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await expect.element(page.getByRole('alert')).toBeVisible();
  expect(page.getByText('当前 Track 还没有对话。', { exact: true }).query()).toBeNull();
  fail = false;
  await page.getByRole('button', { name: '重试', exact: true }).click();
  await expect.poll(() => page.getByRole('alert').query()).toBeNull();
});


it('keeps the closed report composer stable while its conversation is read', async () => {
  await page.viewport(390, 844);
  const errors: string[] = [];
  const original = console.error;
  const spy = vi.spyOn(console, 'error').mockImplementation((...args: unknown[]) => {
    if (args.some((arg) => typeof arg === 'string' && arg.includes('Maximum update depth'))) errors.push(new Error('recursive update').stack ?? 'recursive update');
    else original(...args);
  });
  try {
    let commits = 0;
    const { requests } = renderDailyFixture({ emptyWorkspace: true, onCommit: () => { commits += 1; } });
    await page.getByRole('textbox', { name: 'Chat message', exact: true }).findElement();
    await expect.poll(() => requests.some((request) => request.path.endsWith('/planner/run'))).toBe(true);
    page.getByRole('textbox', { name: 'Chat message', exact: true }).element().focus();
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    await page.getByRole('textbox', { name: 'Chat message', exact: true }).findElement();
    await expect.poll(() => document.querySelector('dialog[open]')).toBeNull();
    const settled = commits;
    await new Promise<void>((resolve) => setTimeout(resolve, 250));
    expect(commits - settled).toBeLessThan(12);
    expect(errors).toEqual([]);
  } finally { spy.mockRestore(); }
});


it.each([320, 390])('switches four sidebar views using current history and real workspace groups at %ipx', async (width) => {
  await page.viewport(width, 844);
  const preferences = createUiPreferences();
  preferences.setReadScope('mobile-sidebar', 1);
  preferences.markUnread('track', 'unread');
  const current = { id: 'project', area_id: 'project-area', title: 'Project evidence', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const pinned = { ...current, id: 'pinned', area_id: 'other-area', title: 'Pinned report', pinned_at: 20 };
  const unread = { ...current, id: 'unread', area_id: 'other-area', title: 'Unread report' };
  const failed = { ...current, id: 'failed', area_id: 'other-area', title: 'Failed report' };
  const working = { ...current, id: 'working', title: 'Working report' };
  const overlays = [
    { id: 'input', plugin_id: 'kernel', entity_kind: 'track', entity_id: current.id, kind: 'activity', payload: { schemaVersion: 3, working: false, attention: 'input', activity_at_ms: null, items: [], cards: [] }, updated_at: 3 },
    { id: 'failure', plugin_id: 'kernel', entity_kind: 'track', entity_id: failed.id, kind: 'activity', payload: { schemaVersion: 3, working: false, attention: 'failed', activity_at_ms: null, items: [], cards: [] }, updated_at: 3 },
    { id: 'running', plugin_id: 'kernel', entity_kind: 'track', entity_id: working.id, kind: 'activity', payload: { schemaVersion: 3, working: true, attention: 'none', activity_at_ms: null, items: [], cards: [] }, updated_at: 3 },
  ];
  const { router, requests } = renderDailyFixture({ initial: '/track/project', uiPreferences: preferences, reply: (request) => {
    if (request.path === '/api/areas') return ok([
      { id: 'project-area', name: 'Project', kind: 'user', color: '#6574cd', sort: 0, created_at: 1, updated_at: 1 },
      { id: 'other-area', name: 'Other', kind: 'user', color: '#6574cd', sort: 1, created_at: 1, updated_at: 1 },
    ]);
    if (request.path === '/api/areas/project-area/tracks') return ok([current, working]);
    if (request.path === '/api/areas/other-area/tracks') return ok([pinned, unread, failed]);
    if (request.path.startsWith('/api/overlays?')) return ok(overlays);
    if (request.path === '/api/tracks/project/conversations') return ok([
      { id: 'current-chat', trackId: 'project', title: 'Current history', kind: 'track-assistant', state: null, updatedAt: 10, lastTurnCompletedAt: null },
    ]);
    if (request.path === '/api/tracks/pinned') return ok({ track: pinned, can_reopen: false, can_close: true, cards: [], overlays: [] });
    return undefined;
  } });
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await expect.element(page.getByRole('button', { name: '本 Track', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await expect.element(page.getByRole('button', { name: 'Current history Project evidence', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '已置顶', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Pinned report', exact: true })).toBeVisible();
  expect(page.getByRole('button', { name: 'Current history Project evidence', exact: true }).query()).toBeNull();
  await page.getByRole('button', { name: '未读', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Unread report', exact: true })).toBeVisible();
  expect(page.getByRole('button', { name: 'Pinned report', exact: true }).query()).toBeNull();
  await page.getByRole('button', { name: '未处理', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'Project evidence, waiting on you', exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Failed report, needs attention', exact: true })).toBeVisible();
  expect(page.getByRole('button', { name: 'Working report, working', exact: true }).query()).toBeNull();
  expect(page.getByRole('button', { name: 'Unread report', exact: true }).query()).toBeNull();
  expect(requests.some((request) => request.path === '/api/tracks/unread/conversations')).toBe(false);
  await page.getByRole('button', { name: '已置顶', exact: true }).click();
  await page.getByRole('button', { name: 'Pinned report', exact: true }).click();
  await expect.poll(() => router.state.location.pathname).toBe('/track/pinned');
  await expect.poll(() => document.querySelector('dialog[open]')).toBeNull();
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await expect.element(page.getByRole('button', { name: '本 Track', exact: true })).toHaveAttribute('aria-pressed', 'true');
  expect(requests.filter((request) => request.method !== 'GET')).toHaveLength(0);
});

it('opens a pinned current report by closing its existing chat without sending its draft', async () => {
  await page.viewport(390, 844);
  const track = { id: 'project', area_id: 'project-area', title: 'Project evidence', sort: 0, cwd: '/tmp', pinned_at: 20, closed_at: null, created_at: 1, updated_at: 2 };
  const { requests } = renderDailyFixture({ initial: '/track/project', reply: request => request.path === '/api/areas/project-area/tracks' ? ok([track]) : undefined });
  await page.getByRole('textbox', { name: 'Chat message', exact: true }).click();
  await page.getByRole('combobox', { name: 'Message', exact: true }).fill('Keep this draft');
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await page.getByRole('button', { name: '已置顶', exact: true }).click();
  await page.getByRole('button', { name: 'Project evidence', exact: true }).click();
  await expect.poll(() => document.querySelector('dialog[open]')).toBeNull();
  await expect.element(page.getByRole('textbox', { name: 'Chat message', exact: true })).toHaveValue('Keep this draft');
  expect(requests.filter(request => request.method !== 'GET')).toHaveLength(0);
});


it('keeps activity read failures distinct from an empty unhandled list and can retry', async () => {
  await page.viewport(390, 844);
  let failed = true;
  renderDailyFixture({ reply: request => request.path.startsWith('/api/overlays?') && failed
    ? { status: 500, statusText: 'Error', body: { error: 'Activity unavailable' } } : undefined });
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await page.getByRole('button', { name: '未处理', exact: true }).click();
  const sidebar = page.getByRole('dialog', { name: '未处理', exact: true });
  await expect.element(sidebar.getByRole('alert')).toBeVisible();
  expect(sidebar.getByText('没有需要处理的 Track。', { exact: true }).query()).toBeNull();
  failed = false;
  await sidebar.getByRole('button', { name: '重试', exact: true }).click();
  await expect.poll(() => sidebar.getByRole('alert').query()).toBeNull();
  await expect.element(sidebar.getByText('没有需要处理的 Track。', { exact: true })).toBeVisible();
});
