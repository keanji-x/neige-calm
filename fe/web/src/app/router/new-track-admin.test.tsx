// @vitest-environment jsdom
// The new-track route's Role: kept per Area draft like the model, locked with it once a create is out, and sent as
// `admin: true` only for Admin, on the first attempt and on the retry of the same request.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(cleanup);

const TASK_LABEL = 'What this track should do';
const UNCONFIRMED_TRACK = 'The track creation is unconfirmed. Try again to check the same track.';
const AREAS = [
  { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
  { id: 'c2', name: 'Reading', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 },
];

/** Every create's answer is lost, so the page keeps the request and offers only its retry. */
function harness() {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = { send(request) {
    sent.push(request);
    if (request.method === 'POST' && request.path === '/api/tracks') return Promise.reject(new Error('Connection closed'));
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.path === '/api/areas' ? AREAS : [] });
  } };
  window.history.pushState({}, '', `${APP_BASEPATH}/`);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }) });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return { creates: () => sent.filter((request) => request.method === 'POST' && request.path === '/api/tracks') };
}

async function openArea(name: string): Promise<void> {
  const today = screen.queryByRole('button', { name: 'Go to Today' });
  if (today !== null) await userEvent.click(today);
  await userEvent.click(await screen.findByRole('button', { name: `New track in ${name}` }));
  await screen.findByLabelText(TASK_LABEL);
}

async function chooseRole(current: string, role: 'Member' | 'Admin'): Promise<void> {
  await userEvent.click(screen.getByRole('button', { name: `Role: ${current}` }));
  await userEvent.click(within(screen.getByRole('menu')).getByRole('menuitem', { name: new RegExp(`^${role}`) }));
}

describe('New track Role', () => {
  it('keeps Admin in its Area draft, locks it with the model, and retries the original admin create', async () => {
    const { creates } = harness();
    await openArea('Work');
    await chooseRole('Member', 'Admin');
    expect(await screen.findByRole('button', { name: 'Role: Admin' })).toBeTruthy();
    await openArea('Reading');
    expect(await screen.findByRole('button', { name: 'Role: Member' })).toBeTruthy();
    await openArea('Work');
    expect(await screen.findByRole('button', { name: 'Role: Admin' })).toBeTruthy();

    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Run the area');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await screen.findByText(UNCONFIRMED_TRACK);
    expect(creates()[0]?.body).toMatchObject({ area_id: 'c1', admin: true, first_message: 'Run the area' });
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Role: Admin' }).disabled).toBe(true);
    expect(screen.getByRole<HTMLButtonElement>('button', { name: /^Model: / }).disabled).toBe(true);

    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(creates()).toHaveLength(2));
    expect(creates()[1]?.body).toEqual(creates()[0]?.body);
  });

  it('sends no `admin` key for the default Member, or after switching back from Admin', async () => {
    const { creates } = harness();
    await openArea('Work');
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Just a track');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(creates()).toHaveLength(1));
    expect(creates()[0]?.body).not.toHaveProperty('admin');

    await openArea('Reading');
    await chooseRole('Member', 'Admin');
    await chooseRole('Admin', 'Member');
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Another track');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(creates()).toHaveLength(2));
    expect(creates()[1]?.body).toMatchObject({ area_id: 'c2', first_message: 'Another track' });
    expect(creates()[1]?.body).not.toHaveProperty('admin');
  });
});
