import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { TaskRecoveryView } from '../../../../core/domain/task-recovery.ts';
import { createIsolatedRetryFixture } from './isolated-task-retry-fixture.tsx';

/** Existing actual-router fixture, with only report/history/file response snapshots changed. */
export function createTaskFileFixture() {
  const base = createIsolatedRetryFixture();
  base.complete();
  const text = '<img src=x onerror="window.fileExecuted=true">\nHello, 世界!';
  const refs = ['/workspace/结果.html', 'binary.bin', 'empty.txt', 'javascript:alert(1)', 'long.txt'];
  const path = `/api/tracks/w1/tasks/${base.taskKey}/attempts/`;
  const fileRequests: ApiRequest[] = [];
  const responses = new Map<string, ApiTransportResponse>();
  let deferred: ((response: ApiTransportResponse) => void) | null = null;
  let hold = false;
  function file(index: number, bytes: Uint8Array, name: string, attemptId = base.queued.attempt_id): ApiTransportResponse {
    return { status: 200, statusText: 'OK', body: { attemptId, index, name, size: bytes.length,
      contentBase64: btoa(Array.from(bytes, (byte) => String.fromCharCode(byte)).join('')) } };
  }
  responses.set(`${path}${base.queued.attempt_id}/artifacts/0`, file(0, new TextEncoder().encode(text), '结果.html'));
  responses.set(`${path}${base.queued.attempt_id}/artifacts/1`, file(1, new Uint8Array([255, 1, 2]), 'binary.bin'));
  responses.set(`${path}${base.queued.attempt_id}/artifacts/2`, file(2, new Uint8Array(), '空.txt'));
  responses.set(`${path}${base.queued.attempt_id}/artifacts/3`, { status: 400, statusText: 'Bad request', body: { error: 'This reported file reference is unsupported.', code: 'bad_request' } });
  responses.set(`${path}${base.queued.attempt_id}/artifacts/4`, file(4, new TextEncoder().encode('字'.repeat(65_537)), 'long.txt'));
  responses.set(`${path}${base.first.attempt_id}/artifacts/0`, file(0, new TextEncoder().encode('Historical bytes.'), 'history.txt', base.first.attempt_id));
  const transport: ApiTransportPort = { async send(request) {
    if (request.path.includes('/artifacts/')) {
      fileRequests.push(request);
      if (hold) return new Promise<ApiTransportResponse>((resolve) => { deferred = resolve; });
      return responses.get(request.path) ?? { status: 404, statusText: 'Not found', body: { error: 'File is unavailable.', code: 'not_found' } };
    }
    const response = await base.transport.send(request);
    if (request.path === `${path.slice(0, -1)}`) {
      const view = response.body as TaskRecoveryView;
      return { ...response, body: { ...view, attempts: view.attempts.map((attempt) => attempt.attempt_id === base.first.attempt_id ? { ...attempt, status: 'done', status_detail: null } : attempt) } };
    }
    if (request.path === `${path}${base.queued.attempt_id}/report`) return { ...response, body: { attemptId: base.queued.attempt_id,
      report: { kind: 'completed', result: 'Reported files are available.', artifacts: refs } } };
    if (request.path === `${path}${base.first.attempt_id}/report`) return { ...response, body: { attemptId: base.first.attempt_id,
      report: { kind: 'completed', result: 'An earlier result.', artifacts: ['history.txt'] } } };
    return response;
  } };
  return { transport, text, refs, fileRequests, base, file,
    response: (index: number, response: ApiTransportResponse) => responses.set(`${path}${base.queued.attempt_id}/artifacts/${index}`, response),
    hold: () => { hold = true; },
    release: (response: ApiTransportResponse) => { if (deferred === null) throw new Error('No file request is waiting.'); deferred(response); },
  };
}
