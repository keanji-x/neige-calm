import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { queryKeys } from '../providers/queries.ts';
import { renderConversationReadFixture } from './conversation-read-fixture.tsx';

afterEach(cleanup);

async function openConversation() {
  fireEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Retained reply.');
}

it.each([true, false])('does not claim Running after a failed run read (initial failure: %s)', async (initialFailure) => {
  const fixture = renderConversationReadFixture(initialFailure);
  await openConversation();
  const field = screen.getByRole('combobox', { name: 'Message' });
  fireEvent.input(field, { target: { textContent: 'Keep this draft.' } });
  if (!initialFailure) {
    await screen.findByText('Running', { exact: true });
    fixture.failRun(true);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun('daily-planner') }); });
  }
  await screen.findByText('Status unconfirmed', { exact: true });
  expect(screen.queryByText('Running', { exact: true })).toBeNull();
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain('The conversation’s status could not be loaded.');
  expect(alert.textContent).not.toContain('private transport diagnostic');
  expect(within(alert).getByRole('button', { name: 'Reload status' })).toBeTruthy();
  expect(field.textContent).toBe('Keep this draft.');
  const before = fixture.requests.length;
  fixture.failRun(false);
  fireEvent.click(within(alert).getByRole('button', { name: 'Reload status' }));
  await screen.findByText('Running', { exact: true });
  await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
  expect(screen.queryByText('Status unconfirmed', { exact: true })).toBeNull();
  expect(screen.getByText('Retained reply.')).toBeTruthy();
  expect(field.textContent).toBe('Keep this draft.');
  const after = fixture.requests.slice(before);
  expect(after.every(request => request.method === 'GET')).toBe(true);
  expect(after.filter(request => !request.path.endsWith('/harness/live')).map(request => request.path))
    .toEqual(['/api/cards/daily-planner/planner/run']);
});

it('retries history independently and retains the last readable transcript', async () => {
  const fixture = renderConversationReadFixture(false);
  await openConversation();
  fixture.failHistory(true);
  await act(async () => { await fixture.client.invalidateQueries({ type: 'active' }); });
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain('The conversation history could not be loaded.');
  expect(screen.getByText('Retained reply.')).toBeTruthy();
  const before = fixture.requests.length;
  fixture.failHistory(false);
  fireEvent.click(within(alert).getByRole('button', { name: 'Reload history' }));
  await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
  expect(fixture.requests.slice(before).every(request => request.method === 'GET' && request.path.includes('/harness/items'))).toBe(true);
  expect(fixture.requests.length).toBeGreaterThan(before);
});

it('keeps execution status unconfirmed throughout a cold retry', async () => {
  const fixture = renderConversationReadFixture(true);
  await openConversation();
  const retry = await screen.findByRole('button', { name: 'Reload status' });
  fixture.failRun(false);
  const release = fixture.pauseRun();
  try {
    fireEvent.click(retry);
    await waitFor(() => expect(fixture.client.isFetching({ queryKey: queryKeys.plannerRun('daily-planner') })).toBe(1));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Reload status' })).toBeNull());
    await screen.findByText('Status unconfirmed', { exact: true });
    expect(screen.getByText('Checking the conversation’s current state.')).toBeTruthy();
    expect(screen.queryByText('Running', { exact: true })).toBeNull();
  } finally { await act(async () => { release(); }); }
  await screen.findByText('Running', { exact: true });
});
