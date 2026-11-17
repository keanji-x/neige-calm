import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import '../../styles/entry.css';
import { ThemeProvider } from '../theme/public.tsx';
import { queryKeys } from '../providers/queries.ts';
import { withReadReceipts } from '../providers/read-receipts-fixture.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { createAppRouter } from './public.tsx';

afterEach(cleanup);

function fixture(deleteStatus: number) {
  let deleted = false;
  const track = { id: 'w1', area_id: 'c1', title: 'Progress deletion', sort: 1, cwd: '/repo',
    workspace: { isolation: 'none', kind: 'attached', path: '/repo', frozen_at: null },
    pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const task = { id: 'task-block', kind: 'task', rev: 1, payload: {
    key: 'completed-build', kind: 'codex', goal: 'Keep this task history.', ready: true, declared_by: 'user',
  } };
  const report = { id: 'report', track_id: 'w1', kind: 'track-report', title: null, sort: 0,
    deletable: false, created_at: 1, updated_at: 2,
    payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: [task] } };
  const worker = { id: 'worker', track_id: 'w1', kind: 'codex', title: 'Build worker', sort: 1,
    payload: {}, deletable: true, created_at: 1, updated_at: 2 };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) { return Promise.resolve().then(() => {
    if (request.method === 'DELETE' && request.path === '/api/cards/worker') {
      if (deleteStatus === 204) { deleted = true; return { status: 204, statusText: 'No Content', body: undefined }; }
      if (deleteStatus === 404) deleted = true;
      return { status: deleteStatus, statusText: 'Delete response', body: {
        error: deleteStatus === 404 ? 'Card not found' : 'Execution is still settling',
        code: deleteStatus === 404 ? 'not_found' : 'conflict',
      } };
    }
    if (request.method !== 'GET') throw new Error(`Unexpected write: ${request.method} ${request.path}`);
    if (request.path === '/api/areas') return ok([{ id: 'c1', name: 'Work', color: '#123456', sort: 1,
      kind: 'user', created_at: 1, updated_at: 2 }]);
    if (request.path === '/api/areas/c1/tracks') return ok([track]);
    if (request.path === '/api/tracks/w1') return ok({ track, planner_revision: 0,
      can_reopen: false, can_admin: false, can_close: true, cards: deleted ? [report] : [report, worker], overlays: [] });
    if (request.path === '/api/tracks/w1/report') return ok({ taskDiagnostics: [{
      blockId: task.id, key: task.payload.key, schedulable: true, status: 'done', workerCardId: worker.id,
      diagnostics: [],
    }] });
    if (request.path === '/api/tracks/w1/tasks/completed-build/attempts') {
      const current = { attempt_id: 'completed-attempt', generation: 1, status: 'done',
        status_detail: null, blocking_reason: null, worker_card_id: worker.id, created_at_ms: 1, finished_at_ms: 2 };
      return ok({ key: task.payload.key, current, attempts: [current] });
    }
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  }); } };
  function mount() {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const router = createAppRouter({ prepareReport: prepareReportDocument, transport: withReadReceipts(transport), client,
      cards: bootTestCardRuntime(), unauthorized: createUnauthorizedChannel({ enqueue: task => task() }),
      onSignOut: () => undefined });
    router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
    const container = document.createElement('div');
    container.id = 'root';
    document.body.append(container);
    const view = render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
      <RouterProvider router={router} />
    </ThemeProvider></QueryClientProvider>, { container });
    return { client, view };
  }
  return { mount };
}

async function deleteWorker() {
  await page.getByRole('button', { name: 'Progress', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'completed-build', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Actions for completed-build' }).click();
  await page.getByRole('menuitem', { name: 'Delete card Build worker' }).click();
  await page.getByRole('button', { name: 'Delete card', exact: true }).click();
}

it.each([204, 404])('removes deleted Progress after HTTP %s and a fresh visit while keeping the report', async status => {
  await page.viewport(1400, 900);
  const world = fixture(status);
  const first = world.mount();
  await deleteWorker();
  await expect.poll(() => document.querySelector('[data-nc-module="work"] [data-nc-row]')).toBeNull();
  await expect.element(page.getByRole('dialog', { name: 'Delete this card?' })).not.toBeInTheDocument();
  expect(first.client.getQueryData<{ cards: { id: string }[] }>(queryKeys.trackDetail('w1'))?.cards.map(card => card.id))
    .toEqual(['report']);
  first.view.unmount();
  first.view.container.remove();
  first.client.clear();
  const fresh = world.mount();
  await expect.poll(() => fresh.client.getQueryData(queryKeys.trackReport('w1'))).toBeDefined();
  await expect.poll(() => fresh.client.isFetching()).toBe(0);
  expect(document.querySelector('[data-nc-module="work"] [data-nc-row]')).toBeNull();
  await page.getByText('Reference', { exact: true }).click();
  await userEvent.click(document.querySelector('#task-block details > summary')!);
  await expect.element(page.getByText('Keep this task history.', { exact: true })).toBeVisible();
  await expect.element(page.getByText('Current attempt 1 · Completed', { exact: true })).toBeVisible();
  if (status === 204) await page.screenshot();
  fresh.client.clear();
});

it('keeps the Progress entry when worker deletion is refused', async () => {
  await page.viewport(1400, 900);
  const { client } = fixture(409).mount();
  await deleteWorker();
  await expect.element(page.getByRole('alert')).toBeVisible();
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect.element(page.getByRole('button', { name: 'completed-build', exact: true })).toBeVisible();
  expect(client.getQueryData<{ cards: { id: string }[] }>(queryKeys.trackDetail('w1'))?.cards.map(card => card.id))
    .toEqual(['report', 'worker']);
  client.clear();
});
