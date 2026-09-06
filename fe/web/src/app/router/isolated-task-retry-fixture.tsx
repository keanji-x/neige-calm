import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render } from '@testing-library/react';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { AcceptedTaskReport } from '../../../../core/domain/independent-task.ts';
import type { TaskAttempt, TaskRecoveryView } from '../../../../core/domain/task-recovery.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

/** Scripted HTTP snapshots only. Stop evidence/admission is the backend's responsibility. */
export function createIsolatedRetryFixture() {
  const taskKey = 'independent-retry-example';
  const goal = 'Calculate six times seven and explain the result.';
  const stoppingReason = 'The previous execution is still stopping. Wait for cleanup.';
  const readyReason = 'Retry this task in a new empty workspace under the unchanged goal.';
  const failureReason = 'The requested calculation could not be completed in attempt one.';
  const first: TaskAttempt = { attempt_id: 'failed-native-attempt', generation: 1, status: 'failed',
    status_detail: 'The independent execution reported failure.', blocking_reason: null, worker_card_id: null,
    created_at_ms: 1788600000000, finished_at_ms: 1788600060000 };
  const queued: TaskAttempt = { attempt_id: 'retry-native-attempt', generation: 2, status: 'pending',
    status_detail: null, blocking_reason: null, worker_card_id: null,
    created_at_ms: 1788600070000, finished_at_ms: null };
  const stopping: TaskRecoveryView = { key: taskKey, current: first, attempts: [first],
    recovery: { allowed: false, code: 'predecessor_not_quiescent', reason: stoppingReason } };
  const stopped: TaskRecoveryView = { ...stopping,
    recovery: { allowed: true, code: 'available', reason: readyReason } };
  const pending: TaskRecoveryView = { key: taskKey, current: queued, attempts: [first, queued],
    recovery: { allowed: false, code: 'not_failed', reason: 'The current attempt has not failed.' } };
  const runningAttempt: TaskAttempt = { ...queued, status: 'running' };
  const doneAttempt: TaskAttempt = { ...queued, status: 'done', finished_at_ms: 1788600120000 };
  const running: TaskRecoveryView = { ...pending, current: runningAttempt, attempts: [first, runningAttempt] };
  const done: TaskRecoveryView = { ...pending, current: doneAttempt, attempts: [first, doneAttempt] };
  const oldReport: AcceptedTaskReport = { attemptId: first.attempt_id, report: { kind: 'failed', reason: failureReason } };
  const completedReport: AcceptedTaskReport = { attemptId: queued.attempt_id,
    report: { kind: 'completed', result: { answer: 42, explanation: 'Six groups of seven make forty-two.' }, artifacts: [] } };
  let history = stopping;
  let currentReport: AcceptedTaskReport = { attemptId: queued.attempt_id, report: null };
  const requests: ApiRequest[] = [];
  const area = { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const track = { id: 'w1', area_id: 'c1', title: 'Retry the independent calculation', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
  const declaration = { id: 'retry-task', rev: 1, kind: 'task', payload: {
    key: taskKey, kind: 'codex', declared_by: 'user', ready: true, goal,
    context: { neige_execution: { version: 'isolated-codex-v1', workspace: 'empty' } },
  } };
  const reportCard = { id: 'report', track_id: 'w1', title: null, kind: 'track-report', sort: 1, deletable: false,
    created_at: 1, updated_at: 2, payload: { schemaVersion: 3, docRev: 7, summary: '', body: '', blocks: [declaration] } };
  const path = `/api/tracks/w1/tasks/${taskKey}`;
  let releaseRecovery: ((response: ApiTransportResponse) => void) | null = null;
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body: JSON.parse(JSON.stringify(body)) });
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    if (request.method === 'POST' && request.path === `${path}/recover`) {
      return new Promise<ApiTransportResponse>((resolve) => { releaseRecovery = resolve; });
    }
    if (request.method !== 'GET') throw new Error(`Unexpected write: ${request.method} ${request.path}`);
    if (request.path === '/api/areas') return Promise.resolve(ok([area]));
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
    if (request.path === '/api/tracks/w1') return Promise.resolve(ok({ track, can_resume: false, cards: [reportCard], overlays: [] }));
    if (request.path === '/api/tracks/w1/report') return Promise.resolve(ok({ ...reportCard.payload, taskDiagnostics: [
      { blockId: declaration.id, key: taskKey, schedulable: true, status: history.current!.status,
        statusDetail: history.current!.status_detail, workerCardId: null, diagnostics: [] },
    ] }));
    if (request.path === `${path}/attempts`) return Promise.resolve(ok(history));
    if (request.path === `${path}/attempts/${first.attempt_id}/report`) return Promise.resolve(ok(oldReport));
    if (request.path === `${path}/attempts/${queued.attempt_id}/report`) return Promise.resolve(ok(currentReport));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    return Promise.resolve(ok([]));
  } };
  return { taskKey, goal, stoppingReason, readyReason, failureReason, first, queued, requests, transport,
    stopped: () => { history = stopped; },
    acceptRecovery: () => {
      if (releaseRecovery === null) throw new Error('Wait for the observed recovery POST before accepting it.');
      history = pending;
      releaseRecovery(ok({ key: taskKey, previous_attempt_id: first.attempt_id, attempt_id: queued.attempt_id, generation: 2 }));
    },
    running: () => { history = running; },
    complete: () => { history = done; currentReport = completedReport; },
  };
}

/** A fresh router and QueryClient on every mount makes reload assertions independent of session cache. */
export function mountIsolatedRetryRoute(transport: ApiTransportPort) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  const view = render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { client, router, dispose: () => { view.unmount(); client.clear(); } };
}
