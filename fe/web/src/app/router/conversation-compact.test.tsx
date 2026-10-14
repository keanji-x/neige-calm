import { cleanup, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { plannerRunOperation } from '../../../../core/domain/conversation.ts';
import { renderCompactFixture } from './conversation-compact-fixture.tsx';

afterEach(cleanup);

it('compact capability is a required boolean in the run contract', () => {
  const { run } = renderCompactFixture('claude', true);
  const schema = plannerRunOperation('daily-planner').responseSchema;
  expect(schema.safeParse(run).success).toBe(true);
  expect(schema.safeParse({ ...run, manual_compact_supported: false }).success).toBe(true);
  const missing: Partial<typeof run> = { ...run };
  delete missing.manual_compact_supported;
  expect(schema.safeParse(missing).success).toBe(false);
  expect(schema.safeParse({ ...run, manual_compact_supported: 'true' }).success).toBe(false);
});

it.each(['claude', 'codex', 'opencode'] as const)('compact uses the declared capability for an existing %s conversation', async provider => {
  const fixture = renderCompactFixture(provider, true);
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Earlier reply.');
  await userEvent.type(screen.getByRole('combobox', { name: 'Message' }), '/compact');
  await screen.findByRole('option', { name: /^compact/ });
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
