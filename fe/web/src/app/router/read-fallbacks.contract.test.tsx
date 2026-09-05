// @vitest-environment jsdom
import { onlineManager, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const areas = [
  { id: 'c1', name: 'One', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
  { id: 'c2', name: 'Two', color: '#654321', sort: 2, kind: 'user', created_at: 1, updated_at: 1 },
];
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const track = { id: 'w1', area_id: 'c1', title: 'Reliable', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
const plannerCard = {
  id: 'planner', track_id: 'w1', kind: 'codex', title: 'Planner', sort: 1,
  payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 1,
};
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const fail = (message: string): ApiTransportResponse => ({ status: 500, statusText: 'Server Error', body: { error: message } });

/* #1253 — none of the cases in this file are about the Today launchpad, and
   `200 null` is that endpoint's ordinary "no launchpad yet" answer. Answering
   it here rather than letting each case's catch-all `ok([])` reach it keeps a
   decode failure out of every Today render below.

   This short-circuit is UNCONDITIONAL: a case's own `reply` never sees this
   path. That is deliberate — one answer for the whole file beats seven copies
   of it — but it means a case that needs the resolve to behave differently has
   to change this wrapper, not its own `reply`. The resolve's own states are
   covered in `today-document.test.tsx`, which is where they belong. */
const TODAY_LAUNCHPAD_PATH = '/api/today/launchpad';
const noLaunchpad = (): ApiTransportResponse => ({ status: 200, statusText: 'OK', body: null });

function renderRoute(path: string, reply: (request: ApiRequest) => ApiTransportResponse | Promise<ApiTransportResponse>) {
  const transport: ApiTransportPort = {
    send: (request) => Promise.resolve(request.path === TODAY_LAUNCHPAD_PATH ? noLaunchpad() : reply(request)),
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  const view = render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { ...view, client };
}

afterEach(() => { cleanup(); onlineManager.setOnline(true); vi.restoreAllMocks(); });

describe('degraded workspace reads stay usable', () => {
  it('mounts navigation while an offline startup Areas query is paused', async () => {
    onlineManager.setOnline(false);
    renderRoute('/', () => ok([]));
    const rail = await screen.findByRole('navigation', { name: 'Workspace' });
    expect(within(rail).getByRole('button', { name: 'Go to Today' })).toBeTruthy();
    expect(within(rail).queryByRole('button', { name: 'Create your first area' })).toBeNull();
  });

  it.each(['Areas', 'Pages'])('provides recovery inside the mobile %s sheet without claiming it is empty', async (section) => {
    const media = window.matchMedia('');
    vi.spyOn(window, 'matchMedia').mockImplementation((query) => ({ ...media, matches: query.includes('width'), media: query }));
    let broken = true;
    renderRoute('/', (request) => {
      if (request.path === '/api/areas') return broken ? fail('Area storage unavailable') : ok(areas);
      if (request.path === '/api/areas/c1/tracks') return ok([track]);
      return ok([]);
    });
    await userEvent.click(await screen.findByRole('button', { name: section }));
    const sheet = screen.getByRole('dialog', { name: section });
    const alert = await within(sheet).findByRole('alert');
    expect(alert.textContent).toContain('Areas are unavailable');
    expect(within(sheet).queryByText('No recent Pages.')).toBeNull();
    broken = false;
    await userEvent.click(within(alert).getByRole('button', { name: 'Retry' }));
    await within(sheet).findByRole('button', { name: section === 'Areas' ? /One/ : /Reliable/ });
    await waitFor(() => expect(within(sheet).queryByRole('alert')).toBeNull());
  });

  it.each(['unavailable', 'malformed'] as const)('keeps navigation and retries an %s Areas startup read', async (failure) => {
    let broken = true;
    renderRoute('/', (request) => {
      if (request.path === '/api/areas') return broken
        ? failure === 'unavailable' ? fail('Area storage temporarily unavailable') : ok({})
        : ok(areas);
      return ok([]);
    });
    const rail = await screen.findByRole('navigation', { name: 'Workspace' });
    const alert = await within(rail).findByRole('alert');
    expect(alert.textContent).toContain('Areas');
    expect(within(rail).queryByRole('button', { name: 'Create your first area' })).toBeNull();
    expect(within(rail).getByRole('button', { name: 'Go to Today' })).toBeTruthy();
    broken = false;
    await userEvent.click(within(alert).getByRole('button', { name: 'Retry' }));
    await within(rail).findByRole('button', { name: 'Collapse area One' });
    await waitFor(() => expect(within(rail).queryByRole('alert')).toBeNull());
  });

  it('keeps cached Areas while a refresh fails and recovers locally', async () => {
    let broken = false;
    const { client } = renderRoute('/', (request) => {
      if (request.path === '/api/areas') return broken ? fail('Area refresh unavailable') : ok(areas);
      return ok([]);
    });
    const rail = await screen.findByRole('navigation', { name: 'Workspace' });
    await within(rail).findByRole('button', { name: 'Collapse area One' });
    broken = true;
    await act(() => client.invalidateQueries({ queryKey: ['areas'] }));
    const alert = await within(rail).findByRole('alert');
    expect(within(rail).getByRole('button', { name: 'Collapse area One' })).toBeTruthy();
    broken = false;
    await userEvent.click(within(alert).getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(within(rail).queryByRole('alert')).toBeNull());
  });

  it('lets an offline Area draft be cancelled without creating it on reconnect', async () => {
    const creates: ApiRequest[] = [];
    const { client } = renderRoute('/', (request) => {
      if (request.path === '/api/areas') {
        if (request.method === 'POST') { creates.push(request); return ok(areas[0]); }
        return ok(areas);
      }
      return ok([]);
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Name' }), 'Offline draft');
    act(() => onlineManager.setOnline(false));
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    const dialog = screen.getByRole('dialog', { name: 'New area' });
    expect((await within(dialog).findByRole('alert')).textContent).toMatch(/offline.*reconnect/i);
    expect(within(dialog).getByRole<HTMLInputElement>('textbox', { name: 'Name' }).value).toBe('Offline draft');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog', { name: 'New area' })).toBeNull();
    await act(async () => { onlineManager.setOnline(true); await client.resumePausedMutations(); });
    expect(creates).toEqual([]);
  });

  it('keeps an offline Area draft editable and creates it once after an explicit online retry', async () => {
    const creates: ApiRequest[] = [];
    const { client } = renderRoute('/', (request) => {
      if (request.path === '/api/areas') {
        if (request.method === 'POST') { creates.push(request); return ok(areas[0]); }
        return ok(areas);
      }
      return ok([]);
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Name' }), 'Offline draft');
    act(() => onlineManager.setOnline(false));
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    const dialog = screen.getByRole('dialog', { name: 'New area' });
    await within(dialog).findByRole('alert');
    const name = within(dialog).getByRole<HTMLInputElement>('textbox', { name: 'Name' });
    await userEvent.clear(name);
    await userEvent.type(name, 'Revised draft');
    await act(async () => { onlineManager.setOnline(true); await client.resumePausedMutations(); });
    expect(creates).toEqual([]);
    await userEvent.click(within(dialog).getByRole('button', { name: 'Create area' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'New area' })).toBeNull());
    expect(creates).toHaveLength(1);
    expect(creates[0]?.body).toMatchObject({ name: 'Revised draft' });
  });

  it('warns on Today when activity is unavailable', async () => {
    renderRoute('/', (request) => {
      if (request.path === '/api/areas') return ok(areas.slice(0, 1));
      if (request.path === '/api/areas/c1/tracks') return ok([track]);
      if (request.path.startsWith('/api/overlays?')) return fail('overlays down');
      return ok([]);
    });
    const main = await screen.findByRole('main');
    expect((await within(main).findAllByRole('alert')).some((node) => node.textContent?.includes('Track activity is unavailable: overlays down'))).toBe(true);
  });

  it('keeps Today content when one area track read fails', async () => {
    renderRoute('/', (request) => {
      if (request.path === '/api/areas') return ok(areas);
      if (request.path === '/api/areas/c1/tracks') return ok([track]);
      if (request.path === '/api/areas/c2/tracks') return fail('area two down');
      return ok([]);
    });
    expect((await screen.findAllByText('Reliable')).length).toBeGreaterThan(1);
    expect(within(screen.getByRole('main')).getAllByRole('alert').some((node) => node.textContent?.includes('area two down'))).toBe(true);
    expect(within(screen.getByRole('main')).getByRole('heading', { level: 1 })).toBeTruthy();
  });

  it('prefers track-detail overlays to the neutral workspace fallback', async () => {
    let resolveDetail: (response: ApiTransportResponse) => void = () => undefined;
    const detail = new Promise<ApiTransportResponse>((resolve) => { resolveDetail = resolve; });
    renderRoute('/track/w1', (request) => {
      if (request.path === '/api/areas') return ok(areas.slice(0, 1));
      if (request.path === '/api/areas/c1/tracks') return ok([track]);
      if (request.path.startsWith('/api/overlays?')) return fail('overlays down');
      if (request.path === '/api/tracks/w1') return detail;
      return ok([]);
    });
    await within(await screen.findByRole('navigation', { name: 'Workspace' })).findByText('Reliable');
    resolveDetail(ok({ track, can_resume: false, cards: [plannerCard], overlays: [{
      id: 'o1', plugin_id: 'kernel', entity_kind: 'track', entity_id: 'w1',
      kind: 'any_card_needs_input', payload: { value: true }, updated_at: 1,
    }, {
      id: 'o2', plugin_id: 'kernel', entity_kind: 'card', entity_id: plannerCard.id,
      kind: 'status', payload: { state: 'AwaitingInput' }, updated_at: 2,
    }] }));
    expect(await screen.findByRole('region', { name: 'Notifications' })).toBeTruthy();
  });

  it('uses a successful neutral detail read instead of stale workspace activity', async () => {
    renderRoute('/track/w1', (request) => {
      if (request.path === '/api/areas') return ok(areas.slice(0, 1));
      if (request.path === '/api/areas/c1/tracks') return ok([track]);
      if (request.path.startsWith('/api/overlays?')) return ok([{
        id: 'workspace-needs-input', plugin_id: 'cards', entity_kind: 'track', entity_id: 'w1',
        kind: 'any_card_needs_input', payload: { value: true }, updated_at: 1,
      }]);
      if (request.path === '/api/tracks/w1') return ok({
        track, can_resume: false, cards: [], overlays: [],
      });
      return ok([]);
    });
    await screen.findByRole('button', { name: 'Rename track' });
    expect(screen.queryByRole('region', { name: 'Notifications' })).toBeNull();
  });
});

it('puts a dismissible delete failure before Today content', async () => {
  renderRoute('/', (request) => {
    if (request.path === '/api/areas') return ok(areas.slice(0, 1));
    if (request.path === '/api/areas/c1/tracks') return ok([track]);
    if (request.path.startsWith('/api/overlays?')) return ok([]);
    if (request.method === 'DELETE') return fail('track changed elsewhere');
    return ok([]);
  });
  const rail = await screen.findByRole('complementary');
  await userEvent.click(await within(rail).findByRole('button', { name: 'Delete Reliable' }));
  await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
  const alert = await screen.findByRole('alert');
  const todayContent = within(screen.getByRole('main')).getByRole('heading', { level: 1 });
  expect(alert.compareDocumentPosition(todayContent) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  await userEvent.click(within(alert).getByRole('button', { name: 'Dismiss' }));
  expect(screen.queryByRole('alert')).toBeNull();
});
