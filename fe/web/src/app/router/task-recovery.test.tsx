import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { TaskAttempt } from '../../../../core/domain/task-recovery.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(cleanup);

function setup(mode: 'success' | 'lost' | 'conflict' | 'blocked' | 'awaiting' | 'dispatched' | 'refresh-fails' = 'success') {
  const requests: ApiRequest[] = [];
  const old: TaskAttempt = { attempt_id: 'opaque-old-id', generation: 1, status: 'failed', status_detail: 'gate-red',
    worker_card_id: null, created_at_ms: 1000, finished_at_ms: 2000 };
  const next: TaskAttempt = { ...old, attempt_id: 'opaque-new-id', generation: 2, status: 'pending',
    status_detail: null, finished_at_ms: null };
  let current = mode === 'awaiting' ? { ...next, status: 'awaiting_projection' }
    : mode === 'dispatched' ? { ...next, status: 'dispatched' } : old;
  let writes = 0;
  const area = { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const track = { id: 'w1', area_id: 'c1', title: 'Continuing work', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
  const card = { id: 'report', track_id: 'w1', title: null, kind: 'track-report', sort: 1, deletable: false,
    created_at: 1, updated_at: 2, payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: [
      { id: 'b-task', rev: 1, kind: 'task', payload: {
        key: 'B', kind: 'codex', declared_by: 'spec', ready: true, goal: 'Complete B under the original requirements.',
      } },
    ] } };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    return Promise.resolve().then(() => {
    requests.push(request);
    if (request.path === '/api/areas') return ok([area]);
    if (request.path === '/api/areas/c1/tracks') return ok([track]);
    if (request.path === '/api/tracks/w1') return ok({ track, can_resume: false, cards: [card], overlays: [] });
    if (request.path === '/api/tracks/w1/report') return ok({ taskDiagnostics: [
      { blockId: 'b-task', key: 'B', schedulable: true, status: current.status === 'awaiting_projection' ? null : current.status, statusDetail: current.status_detail, diagnostics: [] },
    ] });
    if (request.path.endsWith('/attempts') && mode === 'refresh-fails' && writes > 0) return { status: 503, statusText: 'Unavailable', body: { error: 'History temporarily unavailable.', code: 'service_unavailable' } };
    if (request.path.endsWith('/attempts')) return ok({ key: 'B', current,
      attempts: current === old ? [old] : [old, current],
      recovery: { allowed: current === old && mode !== 'blocked', code: mode === 'blocked' ? 'predecessor_not_quiescent' : 'available',
        reason: mode === 'blocked' ? 'The previous worker is still stopping. Wait for cleanup.' : 'Recover under the unchanged contract.' },
    });
    if (request.path.endsWith('/recover')) {
      writes += 1;
      current = next;
      if (mode === 'conflict') return { status: 409, statusText: 'Conflict', body: { code: 'conflict', error: 'A newer attempt already exists.' } };
      if (mode === 'lost' && writes === 1) throw new Error('response lost after commit');
      return ok({ key: 'B', previous_attempt_id: old.attempt_id, attempt_id: next.attempt_id, generation: 2 });
    }
    if (request.path === '/api/settings') return ok({});
    return ok([]);
    });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: vi.fn() });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  const mount = () => render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  mount();
  return { requests, client, mount, open: async () => {
    await userEvent.click(await screen.findByText('Reference'));
    await userEvent.click(document.querySelector('[data-nc-task-state] > summary')!);
  } };
}

it('recovers one business task using the server attempt and shows queued execution with navigable history', async () => {
  const { requests, open } = setup();
  await open();
  await userEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
  expect(await screen.findByText('Recovery requested. A new attempt is queued for preparation.')).toBeTruthy();
  await waitFor(() => expect(screen.getByText('Current attempt 2 · Queued')).toBeTruthy());
  expect(screen.getByText('1 task')).toBeTruthy();
  await userEvent.click(screen.getByText('Attempt history (2)'));
  await userEvent.click(screen.getByText('Attempt 1 · Failed'));
  expect(screen.getByText('gate-red')).toBeTruthy();
  const write = requests.find((r) => r.method === 'POST')!;
  expect(write.path).toBe('/api/tracks/w1/tasks/B/recover');
  expect(write.body).toMatchObject({ expected_attempt_id: 'opaque-old-id',
    reason: 'User requested a new attempt under the unchanged task requirements.' });
  expect((write.body as { idempotency_key: string }).idempotency_key.length).toBeGreaterThan(0);
});

it('retries an uncertain response with the exact original intent even if the server already advanced', async () => {
  const { requests, open, mount } = setup('lost');
  await open();
  await userEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
  await screen.findByRole('button', { name: 'Retry recovery request' });
  cleanup();
  mount();
  await open();
  await userEvent.click(await screen.findByRole('button', { name: 'Retry recovery request' }));
  expect(await screen.findByText('Current attempt 2 · Queued')).toBeTruthy();
  const writes = requests.filter((r) => r.method === 'POST');
  expect(writes).toHaveLength(2);
  expect(writes[1].body).toEqual(writes[0].body);
});

it('refetches a stale conflict and does not automatically create another recovery', async () => {
  const { requests, open } = setup('conflict');
  await open();
  await userEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
  expect(await screen.findByText('A newer attempt already exists.')).toBeTruthy();
  expect(await screen.findByText('Current attempt 2 · Queued')).toBeTruthy();
  expect(requests.filter((r) => r.method === 'POST')).toHaveLength(1);
  expect(screen.queryByRole('button', { name: 'Recover task' })).toBeNull();
});

it('shows the server blocker without offering recovery or exposing a technical form', async () => {
  await setup('blocked').open();
  expect(await screen.findByText('The previous worker is still stopping. Wait for cleanup.')).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Recover task' })).toBeNull();
  expect(screen.queryByRole('textbox')).toBeNull();
});


it('retains the current allocation when there is no projected execution row', async () => {
  await setup('awaiting').open();
  expect(await screen.findByText('Current attempt 2 · Waiting for admission')).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Recover task' })).toBeNull();
  expect(screen.getByText('1 task')).toBeTruthy();
});

it('describes dispatch as preparation before the worker starts', async () => {
  await setup('dispatched').open();
  expect(await screen.findByText('Current attempt 2 · Preparing')).toBeTruthy();
  expect(screen.queryByText('Current attempt 2 · Running')).toBeNull();
});

it('keeps an accepted receipt when history refresh fails and cannot resubmit against stale failed state', async () => {
  await setup('refresh-fails').open();
  await userEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
  expect(await screen.findByText('Recovery requested. A new attempt is queued for preparation.')).toBeTruthy();
  expect(await screen.findByText(/Could not refresh execution history: History temporarily unavailable/)).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Recover task' })).toBeNull();
});
