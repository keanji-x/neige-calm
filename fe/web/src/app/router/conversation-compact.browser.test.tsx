import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderCompactFixture } from './conversation-compact-fixture.tsx';

afterEach(cleanup);

it.each(['claude', 'codex', 'opencode'] as const)('compact in the real %s router composer posts an operation rather than chat', async provider => {
  await page.viewport(1280, 800);
  const fixture = renderCompactFixture(provider, true);
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('Earlier reply.', { exact: true })).toBeVisible();
  await page.getByRole('combobox', { name: 'Message' }).fill('/compact');
  await expect.element(page.getByRole('option', { name: /^compact/ })).toBeVisible();
  await userEvent.keyboard('{Enter}');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/compact')).length).toBe(1);
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toHaveLength(0);
});

it.each([[false, 1280], [true, 1280], [true, 390]] as const)('compact browser lifecycle confirms the same operation (canonical=%s, width=%i)', async (canonical, width) => {
  await page.viewport(width, 844);
  const fixture = renderCompactFixture('claude', true, true, undefined, canonical);
  fixture.run.token_usage = { used_tokens: 100000, context_window: 200000, percent: 47, at_ms: 1 };
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('Earlier reply.', { exact: true })).toBeVisible();
  // The context gauge is a desktop control; completion notices remain shared.
  if (width === 1280) await expect.element(page.getByRole('img', { name: /100k of 200k/ })).toBeVisible();
  fixture.run.context_usage_invalidated = true;
  for (const status of ['started', 'refused', 'failed', 'unconfirmed', 'interrupted'] as const) {
    fixture.run.compact_outcome = { operation_id: 'op', thread_id: 'thread', turn_id: 'maintenance', status };
    await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
    await expect.element(page.getByText('已压缩', { exact: true })).not.toBeInTheDocument();
  }
  fixture.run.phase = 'wedged';
  fixture.run.runtime_state = 'failed';
  fixture.run.compact_outcome = { operation_id: 'op', thread_id: 'thread', turn_id: 'maintenance', status: 'unconfirmed' };
  await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
  await expect.element(page.getByText('用量待更新', { exact: true })).toBeVisible();
  await expect.element(page.getByText('已压缩', { exact: true })).not.toBeInTheDocument();
  await expect.element(page.getByRole('img', { name: /100k of 200k/ })).not.toBeInTheDocument();
  await page.screenshot({ path: `./__screenshots__/late-compact-held-${canonical}-${width}.png` });
  fixture.run.phase = 'turn_completed';
  fixture.run.runtime_state = 'idle';
  fixture.run.compact_outcome = { operation_id: 'op', thread_id: 'thread', turn_id: 'maintenance', status: 'completed' };
  await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
  await expect.element(page.getByText('已压缩', { exact: true })).toBeVisible();
  expect(fixture.requests.filter(request => request.method === 'POST')).toHaveLength(0);
  await expect.element(page.getByText('Earlier reply.', { exact: true })).toBeVisible();
  await page.screenshot({ path: `./__screenshots__/late-compact-completed-${canonical}-${width}.png` });
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await page.getByRole('button', { name: /Conversation Other conversation/ }).click();
  await expect.element(page.getByText('已压缩', { exact: true })).not.toBeInTheDocument();
  await expect.element(page.getByText('用量待更新', { exact: true })).not.toBeInTheDocument();
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('已压缩', { exact: true })).toBeVisible();
  fixture.run.compact_outcome = null;
  await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
  await expect.element(page.getByText('已压缩', { exact: true })).not.toBeInTheDocument();
  await expect.element(page.getByText('用量待更新', { exact: true })).toBeVisible();
  fixture.run.context_usage_invalidated = false;
  fixture.run.token_usage = { used_tokens: 16000, context_window: 200000, percent: 2, at_ms: 2 };
  await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
  if (width === 1280) await expect.element(page.getByRole('img', { name: /16k of 200k/ })).toBeVisible();
  await expect.element(page.getByText('用量待更新', { exact: true })).not.toBeInTheDocument();
  fixture.run.worker_session_id = 'new-runtime';
  fixture.run.compact_outcome = null;
  fixture.run.token_usage = null;
  await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
  await expect.element(page.getByText('已压缩', { exact: true })).not.toBeInTheDocument();
});

it.each(['opencode', 'grok'] as const)('canonical %s Planner and Assistant expose compact only when declared', async provider => {
  await page.viewport(1280, 800);
  const fixture = renderCompactFixture(provider, provider === 'opencode', true, undefined, true);
  for (const title of ['Daily Planner', 'Other']) {
    await page.getByRole('button', { name: new RegExp(`Conversation ${title} conversation`) }).click();
    await expect.element(page.getByText('Earlier reply.', { exact: true })).toBeVisible();
    await page.getByRole('combobox', { name: 'Message' }).fill('/compact');
    if (provider === 'opencode') {
      await expect.element(page.getByRole('option', { name: /^compact/ })).toBeVisible();
      await page.screenshot({ path: `./__screenshots__/compact-${provider}-${title}.png` });
      await userEvent.keyboard('{Enter}');
      await userEvent.keyboard('{Enter}');
      const card = title === 'Other' ? 'second-planner' : 'daily-planner';
      await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path === `/api/cards/${card}/planner/compact`).length).toBe(1);
    } else {
      await expect.element(page.getByRole('option', { name: /^compact/ })).not.toBeInTheDocument();
      await page.screenshot({ path: `./__screenshots__/compact-${provider}-${title}.png` });
    }
    await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  }
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toHaveLength(0);
  if (provider === 'grok') expect(fixture.requests.filter(request => request.path.endsWith('/planner/compact'))).toHaveLength(0);
});
