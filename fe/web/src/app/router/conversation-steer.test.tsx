import { act, cleanup, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { mountSteerFixture as mount, steerAnswer as answer } from './conversation-steer-fixture.tsx';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });
async function open() {
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByText('Queued words');
  expect(screen.getByRole('button', { name: 'Edit queued message' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Delete this message' })).toBeTruthy();
}
it.each([['codex', true], ['claude', false], ['opencode', false]] as const)(
  'steer follows the managed capability for %s through route assembly', async (provider, steer) => {
    const fixture = mount(provider, () => ok(answer(provider, steer)));
    await open();
    await waitFor(() => expect(fixture.client.getQueryData(['managed-providers'])).toEqual(answer(provider, steer)));
    expect(screen.queryByRole('button', { name: 'Say it now' }) !== null).toBe(steer);
  });
it('steer stays hidden while loading, appears when known, and hides on a stale-data query error', async () => {
  let release!: (response: ApiTransportResponse) => void;
  const managed = new Promise<ApiTransportResponse>(resolve => { release = resolve; });
  let failed = false;
  const fixture = mount('codex', () => failed ? { status: 503, statusText: 'Unavailable', body: { error: 'offline' } } : managed);
  await open();
  expect(screen.queryByRole('button', { name: 'Say it now' })).toBeNull();
  await act(async () => { release(ok(answer('codex', true))); await managed; });
  await screen.findByRole('button', { name: 'Say it now' });
  await act(async () => {
    failed = true;
    await fixture.client.refetchQueries({ queryKey: ['managed-providers'] });
  });
  await waitFor(() => expect(screen.queryByRole('button', { name: 'Say it now' })).toBeNull());
});
it('steer stays hidden when the managed query fails before any answer', async () => {
  mount('codex', () => ({ status: 503, statusText: 'Unavailable', body: { error: 'offline' } }));
  await open();
  expect(screen.queryByRole('button', { name: 'Say it now' })).toBeNull();
});
