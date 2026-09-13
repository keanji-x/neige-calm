import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { act, cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { CardEntry } from '../../systems/cards/public.js';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import '../../styles/entry.css';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

function setup() {
  const tracks = ['a', 'b'].map((id) => ({ id, area_id: 'area', title: `Track ${id}`, sort: 1,
    lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null,
    created_at: 1, updated_at: 2 }));
  const cardsFor = (id: string) => [{ id: `report-${id}`, track_id: id, title: null, kind: 'track-report',
    sort: 0, deletable: false, created_at: 1, updated_at: 2,
    payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: Array.from({ length: 50 }, (_, i) => ({
      id: `paragraph-${i}`, kind: 'prose', rev: 1, payload: { markdown: `Paragraph ${i}. A long report to read and return to.` },
    })) } }, ...Array.from({ length: 8 }, (_, i) => ({ id: `${id}-card-${i}`, track_id: id,
      title: `Surface ${i}`, kind: 'view-state-surface', sort: i + 1, deletable: true,
      created_at: 1, updated_at: 2, payload: {},
    }))];
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    let body: unknown = [];
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = tracks;
    if (request.path === '/api/settings') body = {};
    for (const track of tracks) {
      if (request.path === `/api/tracks/${track.id}`) body = { track, can_resume: false, cards: cardsFor(track.id), overlays: [] };
      if (request.path === `/api/tracks/${track.id}/report`) body = { taskDiagnostics: [] };
    }
    return { status: 200, statusText: 'OK', body };
  } };
  const runtime = bootTestCardRuntime();
  const surface: CardEntry<Readonly<{ type: 'view-state-surface'; id: string }>> = {
    type: 'view-state-surface', component: ({ card }) => <div>{card.id}</div>,
    defaultSize: { w: 12, h: 8, minW: 4, minH: 4 }, title: () => 'Surface', accessibleName: () => 'Surface',
    create: { mode: 'kernel-minted-only' },
    fromKernel: (wire) => wire.kind === 'view-state-surface' ? { type: 'view-state-surface', id: wire.id } : null,
  };
  runtime.registry.register(surface as unknown as CardEntry);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: runtime,
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/a'] }) });
  const container = document.createElement('div');
  container.id = 'root';
  document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container });
  return router;
}

async function settle() {
  for (let i = 0; i < 6; i += 1) await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
}

function reportPage() { return document.querySelector<HTMLElement>('[data-nc-track-page]')!; }
function board() { return document.querySelector<HTMLElement>('[data-nc-card-board]')!; }

it('resumes independent report and grid positions through real sidebar navigation', async () => {
  await page.viewport(1440, 900);
  const router = setup();
  await expect.element(page.getByRole('button', { name: 'Rename track', exact: true })).toBeVisible();
  await settle();
  reportPage().scrollTop = 700;
  await settle();
  expect(reportPage().scrollTop).toBe(700);
  await page.getByRole('button', { name: /^Track Track b,/ }).click();
  await expect.element(page.getByRole('button', { name: 'Rename track', exact: true })).toBeVisible();
  await settle();
  expect(reportPage().scrollTop).toBe(0);
  reportPage().scrollTop = 350;
  await settle();
  await page.getByRole('button', { name: /^Track Track a,/ }).click();
  await settle();
  expect(reportPage().scrollTop).toBe(700);

  await act(() => router.navigate({ to: '/track/a', search: { card: 'a-card-7' } }));
  await settle();
  expect(board().scrollHeight).toBeGreaterThan(board().clientHeight);
  board().scrollTop = 240;
  await settle();
  await page.getByRole('button', { name: /^Track Track b,/ }).click();
  await settle();
  expect(reportPage().scrollTop).toBe(350);
  await page.getByRole('button', { name: /^Track Track a,/ }).click();
  await settle();
  expect(router.state.location.search).toMatchObject({ card: 'a-card-7' });
  expect(board().scrollTop).toBe(240);
  expect(reportPage().scrollTop).toBe(700);

  await page.getByRole('button', { name: /^Track Track b,/ }).click();
  await settle();
  await act(() => router.navigate({ to: '/track/a', search: { card: 'a-card-7' } }));
  await settle();
  expect(board().scrollTop).toBeGreaterThan(240);

});
