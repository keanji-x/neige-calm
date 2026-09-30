import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { TaskAttempt, TaskRecoveryView } from '../../../../core/domain/task-recovery.ts';

/** A read-only Track with one failed task, as scripted HTTP snapshots. Any write is a test failure. */
export function createTrackTaskFixture() {
  const taskKey = 'calculation-example';
  const goal = 'Calculate six times seven and explain the result.';
  const first: TaskAttempt = { attempt_id: 'failed-native-attempt', generation: 1, status: 'failed',
    status_detail: 'The execution reported failure.', blocking_reason: null, worker_card_id: null,
    created_at_ms: 1788600000000, finished_at_ms: 1788600060000 };
  const history: TaskRecoveryView = { key: taskKey, current: first, attempts: [first] };
  const requests: ApiRequest[] = [];
  const area = { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const track = { id: 'w1', area_id: 'c1', title: 'The calculation', sort: 1, cwd: '/tmp',
    pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const declaration = { id: 'calculation-task', rev: 1, kind: 'task', payload: {
    key: taskKey, kind: 'codex', declared_by: 'user', ready: true, goal,
  } };
  const reportCard = { id: 'report', track_id: 'w1', title: null, kind: 'track-report', sort: 1, deletable: false,
    created_at: 1, updated_at: 2, payload: { schemaVersion: 3, docRev: 7, summary: '', body: '', blocks: [declaration] } };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body: JSON.parse(JSON.stringify(body)) });
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    if (request.method !== 'GET') throw new Error(`Unexpected write: ${request.method} ${request.path}`);
    if (request.path === '/api/areas') return Promise.resolve(ok([area]));
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
    if (request.path === '/api/tracks/w1') return Promise.resolve(ok({ track, can_reopen: false, can_close: true, cards: [reportCard], overlays: [] }));
    if (request.path === '/api/tracks/w1/report') return Promise.resolve(ok({ ...reportCard.payload, taskDiagnostics: [
      { blockId: declaration.id, key: taskKey, schedulable: true, status: first.status,
        statusDetail: first.status_detail, workerCardId: null, diagnostics: [] },
    ] }));
    if (request.path === `/api/tracks/w1/tasks/${taskKey}/attempts`) return Promise.resolve(ok(history));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    return Promise.resolve(ok([]));
  } };
  return { goal, requests, transport };
}
