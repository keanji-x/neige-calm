import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { TaskAttempt } from '../../../../core/domain/task-recovery.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { applyEventEffects } from '../events/query-invalidation-adapter.ts';
import { initialEventState, reduceEventFrame } from '../../../../core/events/reducer.ts';
import { wireEventSchema } from '../../../../core/api/schemas.ts';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(cleanup);

function setup(mode: 'success' | 'lost' | 'conflict' | 'blocked' | 'awaiting' | 'dispatched' | 'refresh-fails' | 'stale-report' | 'lost-ahead' | 'event-advance' | 'dependency' | 'withdrawn' | 'contract-blocked' | 'capacity' | 'empty' = 'success') {
  const requests: ApiRequest[] = [];
  const taskKey = mode === 'dependency' ? 'c' : 'b';
  const blocker = mode === 'dependency' ? 'Blocked by b (failed). Recover b before c can continue.'
    : mode === 'withdrawn' ? 'Execution release was withdrawn. Release this task to continue.'
    : mode === 'contract-blocked' ? 'Task requirements changed after recovery was requested. Review the declaration.'
    : mode === 'capacity' ? 'All execution slots are occupied. Waiting for capacity.' : null;
  const old: TaskAttempt = { attempt_id: 'opaque-old-id', generation: 1, status: 'failed', blocking_reason: null, status_detail: 'gate-red',
    worker_card_id: 'old-worker', created_at_ms: 1000, finished_at_ms: 2000 };
  const next: TaskAttempt = { ...old, attempt_id: 'opaque-new-id', generation: 2, status: 'pending',
    blocking_reason: null, status_detail: null, worker_card_id: null, finished_at_ms: null };
  let current = mode === 'awaiting' ? { ...next, status: 'awaiting_projection' }
    : mode === 'dispatched' ? { ...next, status: 'dispatched' } : old;
  if (blocker !== null) current = { ...next,
    status: mode === 'dependency' || mode === 'capacity' ? 'pending' : 'awaiting_projection',
    blocking_reason: blocker,
  };
  let initiallyEmpty = mode === 'empty';
  let writes = 0;
  const area = { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const track = { id: 'w1', area_id: 'c1', title: 'Continuing work', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
  const card = { id: 'report', track_id: 'w1', title: null, kind: 'track-report', sort: 1, deletable: false,
    created_at: 1, updated_at: 2, payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: [
      { id: 'b-task', rev: 1, kind: 'task', payload: {
        key: taskKey, kind: 'codex', declared_by: 'user', ready: mode !== 'withdrawn' && !initiallyEmpty,
        goal: `Complete ${taskKey} under the original requirements.`, depends_on: mode === 'dependency' ? ['b'] : [],
      } },
    ] } };
  if (mode === 'dependency') card.payload.blocks.push({ id: 'b-dependency', rev: 1, kind: 'task', payload: {
    key: 'b', kind: 'codex', declared_by: 'user', ready: true, goal: 'Prepare the input for c.', depends_on: [],
  } });
  const worker = { ...card, id: 'old-worker', kind: 'codex', title: 'Previous worker', deletable: true, payload: {} };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    return Promise.resolve().then(() => {
    requests.push(request);
    if (request.path === '/api/areas') return ok([area]);
    if (request.path === '/api/areas/c1/tracks') return ok([track]);
    if (request.path === '/api/tracks/w1') return ok({ track, can_resume: false, cards: [card, worker, { ...worker, id: 'new-worker', title: 'Current worker' }], overlays: [] });
    if (request.path === '/api/tracks/w1/report' && initiallyEmpty) return ok({ taskDiagnostics: [
      { blockId: 'b-task', key: taskKey, schedulable: false, status: null, statusDetail: null, workerCardId: null, diagnostics: [] },
    ] });
    if (request.path === '/api/tracks/w1/report') return ok({ taskDiagnostics: [
      { blockId: 'b-task', key: taskKey, schedulable: true,
        pendingReason: mode === 'dependency' ? { kind: 'dependencyBlocked', message: blocker, dependencies: ['b'] } : null,
        status: blocker !== null ? (mode === 'dependency' ? 'pending' : current.status === 'awaiting_projection' ? null : current.status) : mode === 'event-advance' ? current.status : mode === 'awaiting' ? null : 'failed', statusDetail: blocker !== null || mode === 'event-advance' ? current.status_detail : 'gate-red', workerCardId: blocker !== null ? null : mode === 'event-advance' ? current.worker_card_id : 'old-worker', diagnostics: [] },
      ...(mode === 'dependency' ? [{ blockId: 'b-dependency', key: 'b', schedulable: true, status: 'failed', statusDetail: 'gate-red', diagnostics: [] }] : []),
    ] });
    if (request.path.endsWith('/attempts') && mode === 'refresh-fails' && writes > 0) return { status: 503, statusText: 'Unavailable', body: { error: 'History temporarily unavailable.', code: 'service_unavailable' } };
    if (request.path.endsWith('/attempts') && initiallyEmpty) return ok({ key: taskKey, current: null, attempts: [],
      recovery: { allowed: false, code: 'not_started', reason: 'No attempts yet.' } });
    if (request.path.endsWith('/attempts')) return ok({ key: taskKey, current,
      attempts: mode === 'empty' ? [current] : current === old ? [old] : mode === 'lost-ahead' ? [old, next, current] : [old, current],
      recovery: { allowed: current === old && mode !== 'blocked', code: mode === 'blocked' ? 'predecessor_not_quiescent' : 'available',
        reason: mode === 'blocked' ? 'The previous worker is still stopping. Wait for cleanup.' : 'Recover under the unchanged contract.' },
    });
    if (request.path.endsWith('/recover')) {
      writes += 1;
      current = mode === 'stale-report' ? { ...next, status: 'dispatched' }
        : mode === 'lost-ahead' ? { ...next, attempt_id: 'attempt-three', generation: 3, status: 'running', worker_card_id: 'new-worker' } : next;
      if (mode === 'conflict') return { status: 409, statusText: 'Conflict', body: { code: 'conflict', error: 'A newer attempt already exists.' } };
      if ((mode === 'lost' || mode === 'lost-ahead') && writes === 1) throw new Error('response lost after commit');
      return ok({ key: taskKey, previous_attempt_id: old.attempt_id, attempt_id: next.attempt_id, generation: 2 });
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
  return { requests, client, router, mount, allocate: () => {
    initiallyEmpty = false;
    current = { ...old, status: 'pending', worker_card_id: null, status_detail: null, finished_at_ms: null };
  }, advance: (status: string) => {
    current = { ...next, status, worker_card_id: 'new-worker' };
    const event = wireEventSchema.parse(status === 'done'
      ? { ev: 'task.completed', data: { idempotency_key: current.attempt_id, result: {}, artifacts: [] } }
      : { ev: 'task.dispatched', data: { idempotency_key: current.attempt_id, kind: 'codex' } });
    applyEventEffects(client, reduceEventFrame(initialEventState(null), {
      type: 'event', event, meta: { id: requests.length + 1, eventVersion: 1 },
    }).effects);
  }, open: async () => {
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
  expect(write.path).toBe('/api/tracks/w1/tasks/b/recover');
  expect(write.body).toMatchObject({ expected_attempt_id: 'opaque-old-id',
    reason: 'User requested a new attempt under the unchanged task requirements.' });
  expect((write.body as { idempotency_key: string }).idempotency_key.length).toBeGreaterThan(0);
});

it('retries an uncertain response with the exact original intent even if the server already advanced', async () => {
  const { requests, open, mount } = setup('lost');
  await open();
  await userEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
  await screen.findByRole('button', { name: 'Retry recovery request' });
  expect(document.querySelector('[data-nc-task-state] > summary')!.textContent).toContain('Awaiting recovery confirmation');
  expect(screen.queryByTitle('Open the worker card for b')).toBeNull();
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
  expect(await screen.findByText('Current attempt 2 · Waiting to start')).toBeTruthy();
  expect(document.querySelector('[data-nc-task-state] > summary')!.textContent).toContain('Waiting to start');
  expect(screen.queryByTitle('Open the worker card for b')).toBeNull();
  expect(screen.queryByRole('button', { name: /b.*failed/i })).toBeNull();
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


it('uses the replacement in header and task navigation while the report stays failed', async () => {
  const { open } = setup('stale-report');
  await open();
  expect(screen.getByTitle('Open the worker card for b')).toBeTruthy();
  await userEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
  await screen.findByText('Current attempt 2 · Preparing');
  const summary = document.querySelector('[data-nc-task-state] > summary')!;
  expect(summary.textContent).toContain('Preparing');
  expect(summary.textContent).not.toContain('failed');
  expect(screen.queryByTitle('Open the worker card for b')).toBeNull();
  expect(screen.queryByRole('button', { name: /b.*failed/i })).toBeNull();
  await userEvent.click(screen.getByText('Attempt history (2)'));
  await userEvent.click(screen.getByText('Attempt 1 · Failed'));
  expect(screen.getByRole('button', { name: 'Open attempt 1' })).toBeTruthy();
});

it('hides stale failure and current worker when an accepted receipt outruns the attempts view', async () => {
  await setup('refresh-fails').open();
  await userEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
  await screen.findByText(/Could not refresh execution history/);
  expect(document.querySelector('[data-nc-task-state] > summary')!.textContent).toContain('Awaiting execution refresh');
  expect(screen.queryByText('Current attempt 1 · Failed')).toBeNull();
  expect(screen.getByText('Current attempt 2 · Awaiting execution refresh')).toBeTruthy();
  expect(screen.queryByTitle('Open the worker card for b')).toBeNull();
  expect(screen.queryByRole('button', { name: /b.*failed/i })).toBeNull();
});

it('keeps the newer view when replay returns an older recovery receipt', async () => {
  const { open, router } = setup('lost-ahead');
  await open();
  await userEvent.click(await screen.findByRole('button', { name: 'Recover task' }));
  await userEvent.click(await screen.findByRole('button', { name: 'Retry recovery request' }));
  await screen.findByText('Current attempt 3 · Running');
  expect(document.querySelector('[data-nc-task-state] > summary')!.textContent).toContain('Running');
  expect(screen.queryByText('Current attempt 2 · Queued')).toBeNull();
  expect(screen.queryByText('Recovery requested. A new attempt is queued for preparation.')).toBeNull();
  await userEvent.click(screen.getByTitle('Open the worker card for b'));
  await waitFor(() => expect(router.state.location.href).toContain('card=new-worker'));
});


it('refreshes collapsed current execution through task events without reopening history', async () => {
  const { open, advance, router } = setup('event-advance');
  await open();
  await screen.findByText('Current attempt 1 · Failed');
  const disclosure = document.querySelector<HTMLDetailsElement>('[data-nc-task-state]')!;
  await userEvent.click(disclosure.querySelector('summary')!);
  await waitFor(() => expect(disclosure.open).toBe(false));
  await act(() => { advance('running'); return Promise.resolve(); });
  await waitFor(() => expect(disclosure.querySelector('summary')!.textContent).toContain('Running'));
  expect(disclosure.open).toBe(false);
  expect(screen.getByTitle('1 active')).toBeTruthy();
  await userEvent.click(screen.getByTitle('Open the worker card for b'));
  await waitFor(() => expect(router.state.location.href).toContain('card=new-worker'));
  await act(() => { advance('done'); return Promise.resolve(); });
  await waitFor(() => expect(disclosure.querySelector('summary')!.textContent).toContain('Completed'));
  expect(screen.getByTitle('1 done')).toBeTruthy();
  expect(disclosure.open).toBe(false);
});


it('keeps the current failed dependency explanation after loading pending task history', async () => {
  const { open, advance } = setup('dependency');
  const cause = 'Blocked by b (failed). Recover b before c can continue.';
  expect(await screen.findByTitle(cause)).toBeTruthy();
  await open();
  await screen.findByText('Current attempt 2 · Queued');
  expect(screen.getByText(cause)).toBeTruthy();
  expect(document.querySelector('[data-nc-task-state] > summary [title]')!.getAttribute('title')).toContain(cause);
  expect(screen.getByTitle(cause)).toBeTruthy();
  await act(() => { advance('running'); return Promise.resolve(); });
  await screen.findByText('Current attempt 2 · Running');
  expect(screen.queryByText(cause)).toBeNull();
  expect(screen.queryByTitle(cause)).toBeNull();
});

it.each([
  ['withdrawn', 'Execution release was withdrawn. Release this task to continue.'],
  ['contract-blocked', 'Task requirements changed after recovery was requested. Review the declaration.'],
  ['capacity', 'All execution slots are occupied. Waiting for capacity.'],
] as const)('explains current admission blocker %s without requiring a projected row', async (mode, cause) => {
  await setup(mode).open();
  await screen.findByText(mode === 'capacity' ? 'Current attempt 2 · Queued' : 'Current attempt 2 · Waiting to start');
  expect(screen.getByText(cause)).toBeTruthy();
  expect(document.querySelector('[data-nc-task-state] > summary [title]')!.getAttribute('title')).toContain(cause);
  expect(screen.getByTitle(cause)).toBeTruthy();
});

it('opens never-allocated task history without an alert and refreshes its first allocation', async () => {
  const { open, allocate, requests } = setup('empty');
  await open();
  expect(await screen.findByText('No attempts yet')).toBeTruthy();
  expect(screen.queryByRole('alert')).toBeNull();
  expect(screen.queryByRole('button', { name: 'Recover task' })).toBeNull();
  allocate();
  await userEvent.click(screen.getByRole('button', { name: 'Refresh execution history' }));
  expect(await screen.findByText('Current attempt 1 · Queued')).toBeTruthy();
  expect(screen.queryByText('No attempts yet')).toBeNull();
  expect(screen.queryByRole('alert')).toBeNull();
  expect(requests.filter((request) => request.method === 'POST')).toHaveLength(0);
});
