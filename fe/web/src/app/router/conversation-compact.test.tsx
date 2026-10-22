import { act, cleanup, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { plannerRunOperation } from '../../../../core/domain/conversation.ts';
import { renderCompactFixture } from './conversation-compact-fixture.tsx';

afterEach(cleanup);

it('compact capability is a required boolean in the run contract', () => {
  const { run } = renderCompactFixture('claude', true);
  const schema = plannerRunOperation('daily-planner').responseSchema;
  expect(schema.safeParse(run).success).toBe(true);
  expect(schema.safeParse({ ...run, recovery: null, manual_compact_supported: false }).success).toBe(true);
  const missing: Partial<typeof run> = { ...run };
  delete missing.manual_compact_supported;
  expect(schema.safeParse(missing).success).toBe(false);
  expect(schema.safeParse({ ...run, recovery: null, manual_compact_supported: 'true' }).success).toBe(false);
  for (const field of ['compact_outcome', 'context_usage_invalidated'] as const) {
    const missing = { ...run } as Partial<typeof run>;
    delete missing[field];
    expect(schema.safeParse(missing).success).toBe(false);
  }
});

it.each(['claude', 'codex', 'opencode'] as const)('compact uses the declared capability for an existing %s conversation', async provider => {
  const fixture = renderCompactFixture(provider, true);
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Earlier reply.');
  await userEvent.type(screen.getByRole('combobox', { name: 'Message' }), '/compact');
  await screen.findByRole('option', { name: /^compact/ });
  await userEvent.keyboard('{Enter}');
  await userEvent.keyboard('{Enter}');
  await waitFor(() => expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/compact'))).toHaveLength(1));
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toHaveLength(0);
});

it.each(['claude', 'codex', 'opencode'] as const)('compact is hidden when the %s session declares no capability', async provider => {
  renderCompactFixture(provider, false);
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Earlier reply.');
  await userEvent.type(screen.getByRole('combobox', { name: 'Message' }), '/');
  expect(screen.queryByRole('option', { name: /^compact/ })).toBeNull();
});

it('compact remains hidden until history is ready', async () => {
  renderCompactFixture('codex', true, false);
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('The conversation history could not be loaded.');
  await userEvent.type(screen.getByRole('combobox', { name: 'Message' }), '/');
  expect(screen.queryByRole('option', { name: /^compact/ })).toBeNull();
});

it('compact remains hidden while editing a previous message', async () => {
  renderCompactFixture('claude', true);
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await userEvent.click(await screen.findByRole('button', { name: 'Edit message' }));
  const field = screen.getByRole('combobox', { name: 'Message' });
  await userEvent.clear(field);
  await userEvent.type(field, '/');
  expect(screen.queryByRole('option', { name: /^compact/ })).toBeNull();
});

it('compact confirmed snapshot shows completion and pending usage when reopened', async () => {
  const fixture = renderCompactFixture('claude', true);
  Object.assign(fixture.run, { compact_outcome: { operation_id: 'op', thread_id: 'thread', turn_id: 'maintenance', status: 'completed' }, context_usage_invalidated: true });
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Earlier reply.');
  expect(await screen.findByText('已压缩')).toBeTruthy();
  expect(await screen.findByText('用量待更新')).toBeTruthy();
});

it.each(['started', 'interrupted', 'failed', 'unconfirmed', 'refused'] as const)('compact %s is never confirmed success after reopening', async status => {
  const fixture = renderCompactFixture('codex', true);
  fixture.run.compact_outcome = { operation_id: 'op', thread_id: 'thread', turn_id: 'maintenance', status };
  fixture.run.context_usage_invalidated = true;
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Earlier reply.');
  expect(screen.queryByText('已压缩')).toBeNull();
  expect(await screen.findByText('用量待更新')).toBeTruthy();
});

it('compact POST started and idle phase never imply success', async () => {
  const fixture = renderCompactFixture('claude', true);
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Earlier reply.');
  await userEvent.type(screen.getByRole('combobox', { name: 'Message' }), '/compact');
  await screen.findByRole('option', { name: /^compact/ });
  await userEvent.keyboard('{Enter}');
  await userEvent.keyboard('{Enter}');
  await waitFor(() => expect(fixture.requests.some(request => request.path.endsWith('/planner/compact'))).toBe(true));
  expect(screen.queryByText('已压缩')).toBeNull();
});

it('compact refusal surfaces an error and keeps the old valid reading', async () => {
  const fixture = renderCompactFixture('claude', true, true, { status: 409, statusText: 'Conflict', body: { error: 'Wait for pending messages and edits to finish before compacting.' } });
  fixture.run.token_usage = { used_tokens: 100000, context_window: 200000, percent: 47, at_ms: 1 };
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Earlier reply.');
  await userEvent.type(screen.getByRole('combobox', { name: 'Message' }), '/compact');
  await screen.findByRole('option', { name: /^compact/ });
  await userEvent.keyboard('{Enter}');
  await userEvent.keyboard('{Enter}');
  await screen.findByText('Wait for pending messages and edits to finish before compacting.');
  expect(screen.queryByText('已压缩')).toBeNull();
  expect(screen.queryByText('用量待更新')).toBeNull();
});

it('compact lifecycle hides old occupancy, accepts the next reading and clears a new runtime outcome', async () => {
  const fixture = renderCompactFixture('claude', true);
  fixture.run.token_usage = { used_tokens: 100000, context_window: 200000, percent: 47, at_ms: 1 };
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Earlier reply.');
  expect(screen.getByRole('img', { name: /100k of 200k/ })).toBeTruthy();
  fixture.run.compact_outcome = { operation_id: 'op', thread_id: 'thread', turn_id: 'maintenance', status: 'completed' };
  fixture.run.context_usage_invalidated = true;
  await act(() => fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] }));
  expect(await screen.findByText('已压缩')).toBeTruthy();
  expect(await screen.findByText('用量待更新')).toBeTruthy();
  expect(screen.queryByRole('img', { name: /100k of 200k/ })).toBeNull();
  // The backend supersedes feedback when the next user start is accepted.
  fixture.run.compact_outcome = null;
  await act(() => fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] }));
  await waitFor(() => expect(screen.queryByText('已压缩')).toBeNull());
  expect(screen.getByText('用量待更新')).toBeTruthy();
  fixture.run.context_usage_invalidated = false;
  fixture.run.token_usage = { used_tokens: 16000, context_window: 200000, percent: 2, at_ms: 2 };
  await act(() => fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] }));
  expect(await screen.findByRole('img', { name: /16k of 200k/ })).toBeTruthy();
  expect(screen.queryByText('用量待更新')).toBeNull();
  fixture.run.worker_session_id = 'new-runtime';
  fixture.run.compact_outcome = null;
  fixture.run.token_usage = null;
  await act(() => fixture.client.invalidateQueries({ queryKey: ['planner-run', 'daily-planner'] }));
  await waitFor(() => expect(screen.queryByText('已压缩')).toBeNull());
  expect(screen.queryByRole('img', { name: /16k of 200k/ })).toBeNull();
});

it('compact result stays with its conversation when switching cards', async () => {
  const fixture = renderCompactFixture('claude', true);
  fixture.run.compact_outcome = { operation_id: 'op', thread_id: 'thread', turn_id: 'maintenance', status: 'completed' };
  fixture.run.context_usage_invalidated = true;
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  expect(await screen.findByText('已压缩')).toBeTruthy();
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Other conversation/ }));
  await waitFor(() => expect(screen.queryByText('已压缩')).toBeNull());
  expect(screen.queryByText('用量待更新')).toBeNull();
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  expect(await screen.findByText('已压缩')).toBeTruthy();
  expect(await screen.findByText('用量待更新')).toBeTruthy();
});
