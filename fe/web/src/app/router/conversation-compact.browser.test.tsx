import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderCompactFixture } from './conversation-compact-fixture.tsx';

afterEach(cleanup);

it.each(['claude', 'codex'] as const)('compact in the real %s router composer posts an operation rather than chat', async provider => {
  await page.viewport(1280, 800);
  const fixture = renderCompactFixture(provider, true);
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('Earlier reply.', { exact: true })).toBeVisible();
  await page.getByRole('combobox', { name: 'Message' }).fill('/compact');
  await expect.element(page.getByRole('option', { name: /^compact/ })).toBeVisible();
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/compact')).length).toBe(1);
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toHaveLength(0);
});

it('compact browser lifecycle shows confirmed result, hides old usage, and restores a new reading', async () => {
  await page.viewport(1280, 800);
  const fixture = renderCompactFixture('claude', true);
  fixture.run.token_usage = { used_tokens: 100000, context_window: 200000, percent: 47, at_ms: 1 };
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('Earlier reply.', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('img', { name: /100k of 200k/ })).toBeVisible();
  fixture.run.context_usage_invalidated = true;
  for (const status of ['started', 'refused', 'failed', 'unconfirmed', 'interrupted'] as const) {
    fixture.run.compact_outcome = { operation_id: 'op', thread_id: 'thread', turn_id: 'maintenance', status };
    await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
    await expect.element(page.getByText('已压缩', { exact: true })).not.toBeInTheDocument();
  }
  await expect.element(page.getByText('用量待更新', { exact: true })).toBeVisible();
  await expect.element(page.getByText('已压缩', { exact: true })).not.toBeInTheDocument();
  await expect.element(page.getByRole('img', { name: /100k of 200k/ })).not.toBeInTheDocument();
  fixture.run.compact_outcome = { operation_id: 'op2', thread_id: 'thread', turn_id: 'maintenance2', status: 'completed' };
  await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
  await expect.element(page.getByText('已压缩', { exact: true })).toBeVisible();
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
  await expect.element(page.getByRole('img', { name: /16k of 200k/ })).toBeVisible();
  await expect.element(page.getByText('用量待更新', { exact: true })).not.toBeInTheDocument();
  fixture.run.worker_session_id = 'new-runtime';
  fixture.run.compact_outcome = null;
  fixture.run.token_usage = null;
  await fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] });
  await expect.element(page.getByText('已压缩', { exact: true })).not.toBeInTheDocument();
});
