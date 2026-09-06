import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { IndependentTaskRequest } from '../../../../core/domain/independent-task.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import '../../styles/entry.css';

afterEach(cleanup);

it('runs one immutable intent through the real goal dialog, navigation and report at desktop and phone widths', async () => {
  const requests: ApiRequest[] = [];
  const track = { id: 'w1', area_id: 'c1', title: 'Independent work', sort: 1, lifecycle: 'draft', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
  const card = { id: 'report', track_id: 'w1', title: null, kind: 'track-report', sort: 1, deletable: false,
    created_at: 1, updated_at: 2, payload: { schemaVersion: 3, docRev: 12, summary: '', body: '', blocks: [] as unknown[] } };
  let requestBody: IndependentTaskRequest | null = null;
  let result: unknown = null;
  let postCount = 0;
  let release: (() => void) | undefined;
  const transport: ApiTransportPort = { async send(request) {
    requests.push(request);
    if (request.method === 'POST') {
      postCount += 1;
      if (postCount === 1) {
        await new Promise<void>((resolve) => { release = resolve; });
        throw new Error('Response unavailable');
      }
      requestBody = request.body as IndependentTaskRequest;
      card.payload.docRev = 13;
      card.payload.blocks = [{ id: 'created-task', rev: 1, kind: 'task', payload: {
        key: requestBody.key, declared_by: 'user', kind: 'codex', ready: true, goal: requestBody.goal,
      } }];
      track.lifecycle = 'working';
      return { status: 200, statusText: 'OK', body: { taskKey: requestBody.key, blockId: 'created-task', docRev: 13 } };
    }
    const area = { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
    if (request.path === '/api/areas') return { status: 200, statusText: 'OK', body: [area] };
    if (request.path === '/api/areas/c1/tracks') return { status: 200, statusText: 'OK', body: [track] };
    if (request.path === '/api/settings') return { status: 200, statusText: 'OK', body: {} };
    if (request.path === '/api/tracks/w1/report') return { status: 200, statusText: 'OK', body: { taskDiagnostics: [] } };
    if (request.path === '/api/tracks/w1') return { status: 200, statusText: 'OK', body: JSON.parse(JSON.stringify({
      track, can_resume: false, cards: [card], overlays: [],
    })) };
    if (request.path.endsWith('/attempts')) {
      const attempt = { attempt_id: 'exact-attempt', generation: 1, status: result === null ? 'running' : 'done',
        status_detail: null, blocking_reason: null, worker_card_id: null, created_at_ms: 1000, finished_at_ms: null };
      return { status: 200, statusText: 'OK', body: { key: requestBody?.key, current: attempt, attempts: [attempt],
        recovery: { allowed: false, code: 'not_failed', reason: 'No recovery available.' } } };
    }
    if (request.path.endsWith('/exact-attempt/report')) return { status: 200, statusText: 'OK', body: { attemptId: 'exact-attempt', report: result } };
    return { status: 200, statusText: 'OK', body: [] };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(), unauthorized, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  const mount = () => render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  await page.viewport(1080, 800);
  const view = mount();
  await page.getByRole('button', { name: 'Run independent task', exact: true }).click();
  await expect.element(page.getByText('Starting this task also starts the Track. Other ready tasks in this Track may run.')).toBeVisible();
  await page.getByRole('textbox', { name: 'Goal' }).fill('Explain the moon.');
  await page.getByRole('button', { name: 'Start task', exact: true }).dblClick();
  expect(postCount).toBe(1);
  await act(() => { release?.(); return Promise.resolve(); });
  await expect.element(page.getByRole('button', { name: 'Retry same request' })).toBeVisible();
  view.unmount();
  card.payload.docRev = 99;
  mount();
  await page.getByRole('button', { name: 'Run independent task', exact: true }).click();
  await expect.element(page.getByRole('textbox', { name: 'Goal' })).toHaveValue('Explain the moon.');
  await page.getByRole('button', { name: 'Retry same request' }).click();
  await expect.element(page.getByText('Current attempt 1 · Running')).toBeVisible();
  const writes = requests.filter((request) => request.method === 'POST');
  expect(writes).toHaveLength(2);
  expect(writes[1].body).toEqual(writes[0].body);
  result = { kind: 'completed', result: '<img src=x onerror="window.pwned=1">', artifacts: ['javascript:alert(1)', '/private/result.txt'] };
  await page.getByRole('button', { name: 'Refresh accepted report' }).click();
  await expect.element(page.getByText('<img src=x onerror="window.pwned=1">', { exact: true })).toBeVisible();
  expect(document.querySelector('img[src="x"]')).toBeNull();
  expect(document.querySelector('a[href="javascript:alert(1)"]')).toBeNull();
  await page.screenshot({ path: '__screenshots__/issue-1501-launch-desktop.png' });
  await page.viewport(390, 844);
  await expect.element(page.getByRole('button', { name: 'Refresh accepted report' })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(390);
  await page.screenshot({ path: '__screenshots__/issue-1501-launch-phone.png' });
  await page.getByRole('button', { name: 'Track actions', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Run independent task' }).click();
  await expect.element(page.getByRole('textbox', { name: 'Goal' })).toBeVisible();
  expect(postCount).toBe(2);
  await userEvent.keyboard('{Escape}');
});
