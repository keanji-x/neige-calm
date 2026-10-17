import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(cleanup);

it('keeps an opened HTML page beside the active conversation without covering it', async () => {
  await page.viewport(1440, 900);
  const fixture = renderDailyFixture({ initial: '/track/daily', reply: request => {
    const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
    if (request.path === '/api/tracks/daily') return ok({
      track: { id: 'daily', area_id: 'daily-area', title: 'Portfolio', sort: 0, cwd: '/tmp',
        pinned_at: null, closed_at: null, created_at: 1, updated_at: 1 },
      can_reopen: false, can_admin: false, can_close: false, overlays: [],
      cards: [
        { id: 'daily-planner', track_id: 'daily', kind: 'codex', title: 'Daily Planner conversation', sort: 1,
          payload: { planner_harness: true, planner_provider: 'codex' }, deletable: false, created_at: 1, updated_at: 1 },
        { id: 'daily-report', track_id: 'daily', kind: 'track-report', title: null, sort: -1,
          payload: { schemaVersion: 3, docRev: 0, summary: '', body: '# Portfolio\n\n[Open page](index.html)' },
          deletable: false, created_at: 1, updated_at: 1 },
      ],
    });
    if (request.path === '/api/tracks/daily/workspace/readfile?path=index.html') return ok({
      path: 'index.html', size: 41, text: '<!doctype html><h1>My portfolio</h1>', truncated: false,
    });
    return undefined;
  } });
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  const conversation = page.getByRole('complementary', { name: 'Daily Planner conversation' });
  await expect.element(conversation).toBeVisible();
  await page.getByRole('button', { name: 'Open page', exact: true }).click();
  const file = page.getByRole('region', { name: 'File index.html', exact: true });
  await expect.poll(() => file.element().querySelector('iframe')).not.toBeNull();
  const frame = file.element().querySelector('iframe');
  expect(frame).not.toBeNull();
  await expect.poll(() => {
    const pageBounds = frame!.getBoundingClientRect();
    const chatBounds = conversation.element().getBoundingClientRect();
    return pageBounds.right - chatBounds.left;
  }).toBeLessThanOrEqual(0);
  expect(frame!.getBoundingClientRect().width).toBeGreaterThan(300);
  await expect.element(conversation.getByRole('combobox', { name: 'Message' })).toBeVisible();
  await page.getByRole('button', { name: 'Back to track', exact: true }).click();
  await expect.element(file).not.toBeInTheDocument();
  await expect.element(conversation.getByRole('combobox', { name: 'Message' })).toBeVisible();
  fixture.client.clear();
});
