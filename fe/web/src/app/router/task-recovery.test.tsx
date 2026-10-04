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
import { TaskRecoveryDetails } from '../../features/report/task/recovery.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(cleanup);

function setup(mode: 'awaiting' | 'dispatched' | 'event-advance' | 'dependency' | 'withdrawn' | 'contract-blocked' | 'capacity' | 'empty') {
  const requests: ApiRequest[] = [];
  const taskKey = mode === 'dependency' ? 'c' : 'b';
  const blocker = mode === 'dependency' ? 'Blocked by b (failed). Declare a new task in place of b before c can continue.'
    : mode === 'withdrawn' ? 'Execution release was withdrawn. Release this task to continue.'
    : mode === 'contract-blocked' ? 'Task requirements changed. Review the declaration.'
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
  const area = { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const track = { id: 'w1', area_id: 'c1', title: 'Continuing work', sort: 1, cwd: '/tmp',
    pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
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
    if (request.path === '/api/tracks/w1') return ok({ track, can_reopen: false, can_close: true, cards: [card, worker, { ...worker, id: 'new-worker', title: 'Current worker' }], overlays: [] });
    if (request.path === '/api/tracks/w1/report' && initiallyEmpty) return ok({ taskDiagnostics: [
      { blockId: 'b-task', key: taskKey, schedulable: false, status: null, statusDetail: null, workerCardId: null, diagnostics: [] },
    ] });
    if (request.path === '/api/tracks/w1/report') return ok({ taskDiagnostics: [
      { blockId: 'b-task', key: taskKey, schedulable: true,
        pendingReason: mode === 'dependency' ? { kind: 'dependencyBlocked', message: blocker, dependencies: ['b'] } : null,
        status: blocker !== null ? (mode === 'dependency' ? 'pending' : current.status === 'awaiting_projection' ? null : current.status) : mode === 'event-advance' ? current.status : mode === 'awaiting' ? null : 'failed', statusDetail: blocker !== null || mode === 'event-advance' ? current.status_detail : 'gate-red', workerCardId: blocker !== null ? null : mode === 'event-advance' ? current.worker_card_id : 'old-worker', diagnostics: [] },
      ...(mode === 'dependency' ? [{ blockId: 'b-dependency', key: 'b', schedulable: true, status: 'failed', statusDetail: 'gate-red', diagnostics: [] }] : []),
    ] });
    if (request.path.endsWith('/attempts') && initiallyEmpty) return ok({ key: taskKey, current: null, attempts: [] });
    if (request.path.endsWith('/attempts')) return ok({ key: taskKey, current,
      attempts: mode === 'empty' ? [current] : current === old ? [old] : [old, current],
    });
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

it('retains the current allocation when there is no projected execution row', async () => {
  await setup('awaiting').open();
  expect(await screen.findByText('Current attempt 2 · Waiting to start')).toBeTruthy();
  expect(document.querySelector('[data-nc-task-state] > summary')!.textContent).toContain('Waiting to start');
  expect(screen.queryByTitle('Open the worker card for b')).toBeNull();
  expect(screen.queryByRole('button', { name: /b.*failed/i })).toBeNull();
  expect(screen.getByText('1 task')).toBeTruthy();
  await userEvent.click(screen.getByText('Attempt history (2)'));
  await userEvent.click(screen.getByText('Attempt 1 · Failed'));
  expect(screen.getByText('gate-red')).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Open attempt 1' })).toBeTruthy();
});

it('describes dispatch as preparation before the worker starts', async () => {
  await setup('dispatched').open();
  expect(await screen.findByText('Current attempt 2 · Preparing')).toBeTruthy();
  expect(screen.queryByText('Current attempt 2 · Running')).toBeNull();
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
  expect(document.querySelector('[data-nc-module="tasks"] [data-nc-inventory-group="working"] summary')?.getAttribute('aria-label')).toBe('In progress, 1 task');
  await userEvent.click(screen.getByTitle('Open the worker card for b'));
  await waitFor(() => expect(router.state.location.href).toContain('card=new-worker'));
  await act(() => { advance('done'); return Promise.resolve(); });
  await waitFor(() => expect(disclosure.querySelector('summary')!.textContent).toContain('Completed'));
  expect(document.querySelector('[data-nc-module="tasks"] [data-nc-inventory-group="done"] summary')?.getAttribute('aria-label')).toBe('Completed, 1 task');
  expect(disclosure.open).toBe(false);
});

it('keeps the current failed dependency explanation after loading pending task history', async () => {
  const { open, advance } = setup('dependency');
  const cause = 'Blocked by b (failed). Declare a new task in place of b before c can continue.';
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
  ['contract-blocked', 'Task requirements changed. Review the declaration.'],
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
  allocate();
  await userEvent.click(screen.getByRole('button', { name: 'Refresh execution history' }));
  expect(await screen.findByText('Current attempt 1 · Queued')).toBeTruthy();
  expect(screen.queryByText('No attempts yet')).toBeNull();
  expect(screen.queryByRole('alert')).toBeNull();
  expect(requests.filter((request) => request.method === 'POST')).toHaveLength(0);
});


it('keeps the history refresh action visible and unavailable while refreshing', async () => {
  const refresh = vi.fn();
  render(<TaskRecoveryDetails view={undefined} current={undefined} loading
    loadError={null} onRefresh={refresh} openWorker={undefined} openableWorkerIds={new Set()} />);
  const action = screen.getByRole('button', { name: 'Refresh execution history' });
  expect(action.getAttribute('aria-busy')).toBe('true');
  await userEvent.click(action);
  expect(refresh).not.toHaveBeenCalled();
});

it('groups a failed history read with its recovery action', () => {
  render(<TaskRecoveryDetails view={undefined} current={undefined} loading={false}
    loadError="History is unavailable." onRefresh={vi.fn()} openWorker={undefined} openableWorkerIds={new Set()} />);
  expect(screen.getByRole('alert').contains(screen.getByRole('button', { name: 'Refresh execution history' }))).toBe(true);
});
