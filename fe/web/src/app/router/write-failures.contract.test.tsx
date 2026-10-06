// @vitest-environment jsdom
// #2131 S1: every non-chat write on these surfaces settles through one runner and reads its failure through the write's
// table. A lost answer shows the write's fixed state, never transport text or a connection sentence, and a DELETE retried
// after one and answered 404 is done.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const area = { id: 'c1', name: 'One', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const openTrack = { id: 'w1', area_id: 'c1', title: 'Reliable', sort: 1, cwd: '/tmp',
  pinned_at: null, closed_at: null, created_at: 1, updated_at: 1 };
const card = { id: 'k1', track_id: 'w1', kind: 'notes', title: 'Build log', sort: 1, payload: {},
  deletable: true, created_at: 1, updated_at: 1 };
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const notFound: ApiTransportResponse = { status: 404, statusText: 'Not Found', body: { error: 'track not found', code: 'not_found' } };
/** What a lost answer, a timeout or an unreadable answer would put on screen if the raw failure were shown. */
const RAW_OR_CONNECTIVITY = /Transport request failed|timed out|schema|offline|reconnect|connection/i;

/** `stale`: track reads stop answering, so only a write's own cache update can change the page. */
type World = { closed: boolean; gone: boolean; stale: boolean };

/** The real app over a fake server: reads answer from `world`, and every write goes to `write`. */
function renderApp(path: string, write: (request: ApiRequest, world: World) => Promise<ApiTransportResponse>, closed = false) {
  const world: World = { closed, gone: false, stale: false };
  const writes: ApiRequest[] = [];
  const reads: string[] = [];
  const transport: ApiTransportPort = { send(request) {
    if (request.method !== 'GET') { writes.push(request); return write(request, world); }
    reads.push(request.path);
    const track = { ...openTrack, closed_at: world.closed ? 2 : null };
    if (request.path === '/api/areas') return Promise.resolve(ok([area]));
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok(world.gone ? [] : [track]));
    if (request.path === '/api/tracks/w1' && world.stale) return new Promise<ApiTransportResponse>(() => undefined);
    if (request.path === '/api/tracks/w1') return Promise.resolve(world.gone ? notFound : ok({
      track, can_reopen: world.closed, can_close: !world.closed, cards: [card], overlays: [],
    }));
    if (request.path === '/api/today/launchpad') return Promise.resolve(ok(null));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    if (request.path === '/api/version') return Promise.resolve(ok({ areaCreateIdempotency: true }));
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { world, writes, reads, router };
}

const lost = (): Promise<ApiTransportResponse> => Promise.reject(new Error('socket hang up'));

async function rail() { return screen.findByRole('navigation', { name: 'Workspace' }); }
async function trackMenu(item: string) {
  await userEvent.click(await screen.findByRole('button', { name: 'Track actions for Reliable' }));
  await userEvent.click(screen.getByRole('menuitem', { name: item }));
}
async function areaMenu(item: string) {
  await userEvent.click(await screen.findByRole('button', { name: 'Area actions for One' }));
  await userEvent.click(screen.getByRole('menuitem', { name: item }));
}
async function deleteFromRail() {
  await userEvent.click(await within(await rail()).findByRole('button', { name: 'Delete Reliable' }));
  await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
}

afterEach(cleanup);

describe('a lost answer shows the write’s fixed state, never transport text', () => {
  /* Name, route, press, the fixed state shown; the track starts closed only where the press is Reopen. */
  const surfaces: ReadonlyArray<readonly [string, string, () => Promise<void>, string]> = [
    ['sidebar pin', '/today/legacy', async () => {
      await userEvent.click(await within(await rail()).findByRole('button', { name: 'Pin Reliable' }));
    }, 'The pin change is unconfirmed.'],
    ['rename', '/track/w1', async () => {
      await userEvent.click(await screen.findByRole('button', { name: 'Rename track' }));
      await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
      await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), 'Renamed{Enter}');
    }, 'The rename is unconfirmed.'],
    ['close', '/track/w1', () => trackMenu('Close'), 'Closing the track is unconfirmed.'],
    ['reopen', '/track/w1', () => trackMenu('Reopen'), 'Reopening the track is unconfirmed.'],
    ['track delete', '/today/legacy', deleteFromRail, 'The delete is unconfirmed.'],
    ['area delete', '/today/legacy', async () => {
      await areaMenu('Delete area');
      await userEvent.type(screen.getByRole('textbox', { name: 'Type One to confirm.' }), 'One');
      await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    }, 'The delete is unconfirmed.'],
    ['card delete', '/track/w1', async () => {
      await userEvent.click(await screen.findByRole('button', { name: 'Delete card Build log' }));
      await userEvent.click(screen.getByRole('button', { name: 'Delete card' }));
    }, 'The delete is unconfirmed.'],
    ['area edit', '/today/legacy', async () => {
      await areaMenu('Edit area');
      await userEvent.clear(screen.getByRole('textbox', { name: /^Name/ }));
      await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Renamed');
      await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    }, 'The area update is unconfirmed.'],
    /* The creates keep S0's key handling; a lost answer is their fixed unconfirmed state. */
    ['track create', '/today/legacy', async () => {
      await userEvent.click(await screen.findByRole('button', { name: 'New track in One' }));
      await userEvent.type(await screen.findByLabelText('What this track should do'), 'Ship it');
      await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    }, 'The track creation is unconfirmed. Try again to check the same track.'],
    ['area create', '/today/legacy', async () => {
      await userEvent.click(await screen.findByRole('button', { name: 'New area' }));
      await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Two');
      await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    }, 'Creation could not be confirmed. Try again to safely check the same area.'],
    ['Today ensure', '/today/legacy', async () => {
      await userEvent.click(await screen.findByRole('button', { name: 'Start a conversation with Today' }));
    }, 'Starting Today assistant is unconfirmed.'],
  ];

  it.each(surfaces)('%s', async (name, path, press, fixed) => {
    const { writes } = renderApp(path, lost, name === 'reopen');
    await press();
    await waitFor(() => expect(screen.getAllByRole('alert').map((alert) => alert.textContent)).toContainEqual(expect.stringContaining(fixed)));
    expect(writes.length).toBeGreaterThan(0);
    for (const alert of screen.getAllByRole('alert')) expect(alert.textContent).not.toMatch(RAW_OR_CONNECTIVITY);
  });
});

it('reads the track again after a patch whose answer was lost: it may have been stored', async () => {
  const { reads } = renderApp('/track/w1', lost);
  await screen.findByRole('button', { name: 'Track actions for Reliable' });
  await waitFor(() => expect(reads).toContain('/api/tracks/w1'));
  const before = reads.filter((path) => path === '/api/tracks/w1').length;
  await trackMenu('Close');
  await screen.findByText('Closing the track is unconfirmed.');
  await waitFor(() => expect(reads.filter((path) => path === '/api/tracks/w1').length).toBeGreaterThan(before));
});

describe('a DELETE retried after a lost answer and answered 404 is done', () => {
  it('drops the track from the rail with no error', async () => {
    let attempt = 0;
    const { writes } = renderApp('/today/legacy', (_request, world) => {
      /* The first delete landed but its answer was lost, and reads have not caught up yet; the retry meets the 404. */
      attempt += 1;
      if (attempt === 1) return lost();
      world.gone = true;
      return Promise.resolve(notFound);
    });
    await deleteFromRail();
    expect(within(await screen.findByRole('alert')).getByText('The delete is unconfirmed.')).toBeTruthy();
    await deleteFromRail();
    await waitFor(() => expect(within(screen.getByRole('navigation', { name: 'Workspace' })).queryByText('Reliable')).toBeNull());
    expect(screen.queryAllByRole('alert').map((alert) => alert.textContent)).toEqual([]);
    expect(writes.map((request) => `${request.method} ${request.path}`)).toEqual(['DELETE /api/tracks/w1', 'DELETE /api/tracks/w1']);
  });

  /* #2131 S2: the board draws the cached detail, so a done delete drops the card there rather than waiting for a read. */
  it('drops the card from the board before any read answers', async () => {
    let attempt = 0;
    const { writes } = renderApp('/track/w1', (_request, world) => {
      attempt += 1;
      if (attempt === 1) return lost();
      world.stale = true;
      return Promise.resolve({ status: 404, statusText: 'Not Found', body: { error: 'card not found', code: 'not_found' } });
    });
    const deleteCard = async () => {
      await userEvent.click(await screen.findByRole('button', { name: 'Delete card Build log' }));
      await userEvent.click(screen.getByRole('button', { name: 'Delete card' }));
    };
    await deleteCard();
    expect(within(await screen.findByRole('alert')).getByText('The delete is unconfirmed.')).toBeTruthy();
    await deleteCard();
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Delete card Build log' })).toBeNull());
    expect(screen.queryAllByRole('alert').map((alert) => alert.textContent)).toEqual([]);
    expect(writes.map((request) => `${request.method} ${request.path}`)).toEqual(['DELETE /api/cards/k1', 'DELETE /api/cards/k1']);
  });

  it('leaves the deleted track’s own page as a delete that held', async () => {
    let attempt = 0;
    const { router } = renderApp('/track/w1', (_request, world) => {
      /* The first delete landed but its answer was lost, and reads have not caught up yet; the retry meets the 404. */
      attempt += 1;
      if (attempt === 1) return lost();
      world.gone = true;
      return Promise.resolve(notFound);
    });
    await trackMenu('Delete track');
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
    expect((await screen.findByText('The delete is unconfirmed.'))).toBeTruthy();
    await trackMenu('Delete track');
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/'));
    expect(screen.queryByText('The delete is unconfirmed.')).toBeNull();
  });
});
