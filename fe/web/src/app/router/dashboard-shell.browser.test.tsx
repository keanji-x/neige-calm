// Register the production cascade before importing any component CSS modules.
import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { act, cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { CardEntry } from '../../systems/cards/public.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import source from '../../../../../test-data/native-view-v1.json?raw';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

function setup({ native = true, inventory = false, initial = '/track/a' } = {}) {
  const tracks = ['a', 'b'].map(id => ({ id, area_id: 'area', title: `Track ${id}`, sort: 1,
    lifecycle: 'working', cwd: '/tmp', archived_at: null, pinned_at: null, terminal_at: null,
    created_at: 1, updated_at: 2 }));
  const fixture = JSON.parse(source) as { valid: unknown };
  const cardsFor = (id: string) => [{ id: `report-${id}`, track_id: id, title: null, kind: 'track-report',
    sort: 0, deletable: false, created_at: 1, updated_at: 2,
    payload: { summary: '', body: '', blocks: [
      ...(native && id === 'a' ? [{ id: 'native-view', kind: 'view', rev: 1, payload: fixture.valid }] : []),
      ...Array.from({ length: 40 }, (_, i) => ({ id: `paragraph-${i}`, kind: 'prose', rev: 1,
        payload: { markdown: `Paragraph ${i}. A report to read and return to.` } })),
    ] } }, ...(inventory ? [
      { id: `planner-${id}`, track_id: id, kind: 'codex', title: 'Planner chat', sort: 1,
        deletable: false, created_at: 1, updated_at: 2, payload: { planner_harness: true } },
      { id: `surface-${id}`, track_id: id, kind: 'shell-test-surface', title: 'Notes', sort: 2,
        deletable: true, created_at: 1, updated_at: 2, payload: {} },
    ] : [])];
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    let body: unknown = [];
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = tracks;
    if (request.path === '/api/settings') body = {};
    for (const track of tracks) {
      if (request.path === `/api/tracks/${track.id}`) body = { track, can_resume: false, cards: cardsFor(track.id), overlays: [] };
      if (request.path === `/api/tracks/${track.id}/report`) body = { taskDiagnostics: [] };
      if (request.path === `/api/cards/planner-${track.id}/planner/run`) body = {
        card_id: `planner-${track.id}`, worker_session_id: 'fixture', phase: 'idle',
        model: null, reasoning_effort: null, blocked_reason: null,
      };
    }
    return { status: 200, statusText: 'OK', body };
  } };
  const runtime = bootTestCardRuntime();
  const surface: CardEntry<Readonly<{ type: 'shell-test-surface'; id: string }>> = {
    type: 'shell-test-surface', component: () => <p>Board notes</p>,
    defaultSize: { w: 12, h: 8, minW: 4, minH: 4 }, title: () => 'Notes', accessibleName: () => 'Notes',
    create: { mode: 'kernel-minted-only' },
    fromKernel: wire => wire.kind === 'shell-test-surface' ? { type: 'shell-test-surface', id: wire.id } : null,
  };
  runtime.registry.register(surface as unknown as CardEntry);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: runtime,
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [initial] }) });
  const container = document.createElement('div');
  container.id = 'root';
  document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container });
  return router;
}

function reportPage() { return document.querySelector<HTMLElement>('[data-nc-track-page]')!; }
function nativeBlock() { return document.getElementById('native-view')!; }
function inventoryPanel() { return document.querySelector<HTMLElement>('[data-nc-panel]')!; }
async function ready() {
  await expect.element(page.getByRole('button', { name: 'Rename track', exact: true })).toBeVisible();
}

it('gives declared native reports the available width at 1440 without an empty inventory column', async () => {
  await page.viewport(1440, 900);
  setup();
  await ready();
  expect(nativeBlock().getBoundingClientRect().width).toBeGreaterThanOrEqual(1080);
  expect(nativeBlock().getBoundingClientRect().right).toBeLessThanOrEqual(1440);
  expect(inventoryPanel().getBoundingClientRect().width).toBe(0);
  expect(document.querySelector('[data-nc-report-outline]')).toBeNull();
  await expect.element(page.getByRole('button', { name: 'Show track panel' })).toHaveAttribute('aria-expanded', 'false');
  expect(document.querySelector('iframe')).toBeNull();
  await page.screenshot({ path: 'test-results/dashboard-shell-1440.png' });
});

it('preserves the native reader and inventory nodes through panel, Planner, and board round trips', async () => {
  await page.viewport(1440, 900);
  setup({ inventory: true });
  await ready();
  const report = nativeBlock();
  const panel = inventoryPanel();
  await page.getByRole('button', { name: '合计', exact: true }).click();
  const dataset = page.getByRole('button', { name: '合计', exact: true });
  await expect.element(dataset).toHaveAttribute('aria-pressed', 'true');
  await page.getByRole('button', { name: 'Show track panel' }).click();
  await expect.element(page.getByRole('heading', { name: 'Cards', exact: true })).toBeVisible();
  await expect.element(page.getByRole('heading', { name: 'Tasks', exact: true })).toBeVisible();
  await page.getByRole('button', { name: /^Conversation Planner chat/ }).click();
  await expect.element(page.getByRole('complementary', { name: 'Planner chat' })).toBeVisible();
  const drawer = document.querySelector('[data-nc-drawer]')!.getBoundingClientRect();
  expect(report.getBoundingClientRect().right).toBeLessThanOrEqual(drawer.left);
  await page.getByRole('button', { name: 'Close conversation' }).click();
  await page.getByText('Available', { exact: true }).click();
  await page.getByRole('button', { name: /^Notes/ }).click();
  await expect.element(page.getByText('Board notes', { exact: true })).toBeVisible();
  expect(report.closest('[inert]')).not.toBeNull();
  await page.getByRole('button', { name: 'Back to track' }).click();
  await page.getByRole('button', { name: 'Hide track panel' }).click();
  await page.getByRole('button', { name: 'Show track panel' }).click();
  await expect.element(page.getByRole('button', { name: /^Notes/ })).toBeVisible();
  await page.getByRole('button', { name: 'Hide track panel' }).click();
  expect(nativeBlock()).toBe(report);
  expect(inventoryPanel()).toBe(panel);
  await expect.element(dataset).toHaveAttribute('aria-pressed', 'true');
  expect(report.getBoundingClientRect().width).toBeGreaterThanOrEqual(1080);
});

it('keeps ordinary document routes unchanged after leaving a dashboard', async () => {
  await page.viewport(1440, 900);
  const router = setup();
  await ready();
  await page.getByRole('button', { name: 'Show track panel' }).click();
  await page.getByRole('button', { name: 'Hide track panel' }).click();
  await act(() => router.navigate({ to: '/track/b' }));
  await ready();
  expect(inventoryPanel().getBoundingClientRect().width).toBeGreaterThanOrEqual(240);
  await expect.element(page.getByRole('heading', { name: 'Cards', exact: true })).toBeVisible();
  const prose = document.getElementById('paragraph-0')!.getBoundingClientRect();
  expect(prose.width).toBeLessThanOrEqual(568);
});

it.each([390, 736])('keeps mobile panel navigation and the native reader at %i', async width => {
  await page.viewport(width, 900);
  const router = setup({ inventory: true, initial: '/track/a?panel=conversations' });
  await expect.element(page.getByRole('button', { name: /^Conversation Planner chat/ })).toBeVisible();
  expect(inventoryPanel().getBoundingClientRect().width).toBe(width);
  await page.getByRole('button', { name: 'Back to Report', exact: true }).click();
  expect(router.state.location.search).not.toHaveProperty('panel');
  expect(reportPage().scrollWidth).toBeLessThanOrEqual(reportPage().clientWidth);
  await expect.element(page.getByRole('button', { name: 'Track actions', exact: true })).toBeVisible();
  await page.screenshot({ path: `test-results/dashboard-shell-${width}.png` });
  await page.getByRole('button', { name: 'Track actions', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Cards', exact: true }).click();
  expect(router.state.location.search).toMatchObject({ panel: 'cards' });
  await page.getByRole('group').getByText('Available', { exact: true }).click();
  await expect.element(page.getByRole('group').getByText('Notes', { exact: true })).toBeVisible();
});
