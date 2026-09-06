import { expect, it } from 'vitest';
import { acceptedTaskReportOperation, acceptedTaskReportSchema, independentTaskReceiptSchema,
  independentTaskRevision, startIndependentTaskOperation } from './independent-task.js';

it('uses the fixed start contract without changing the immutable intent', () => {
  const request = Object.freeze({ key: 'independent-nonce', goal: 'Explain gravity', ifDocRev: 12 });
  const op = startIndependentTaskOperation('track/a', request);
  expect(op).toMatchObject({ method: 'POST', path: '/api/tracks/track%2Fa/isolated-tasks', body: request });
  expect(op.body).toBe(request);
  expect(op.responseSchema.safeParse({ taskKey: 'other', blockId: 'b', docRev: 13 }).success).toBe(false);
  expect(independentTaskReceiptSchema.safeParse({ taskKey: request.key, blockId: 'b' }).success).toBe(false);
  expect(independentTaskRevision([])).toBeNull();
});

it('distinguishes absent accepted report from completed JSON null and requires result/artifacts', () => {
  expect(acceptedTaskReportSchema.parse({ attemptId: 'a', report: null }).report).toBeNull();
  expect(acceptedTaskReportSchema.parse({ attemptId: 'a', report: { kind: 'completed', result: null, artifacts: [] } }).report)
    .toEqual({ kind: 'completed', result: null, artifacts: [] });
  for (const report of [{ kind: 'completed', artifacts: [] }, { kind: 'completed', result: null }, { kind: 'failed' }]) {
    expect(acceptedTaskReportSchema.safeParse({ attemptId: 'a', report }).success).toBe(false);
  }
});

it('scopes accepted reports to exact encoded Track, task and attempt', () => {
  const op = acceptedTaskReportOperation('track/a', 'key/b', 'attempt/c');
  expect(op.path).toBe('/api/tracks/track%2Fa/tasks/key%2Fb/attempts/attempt%2Fc/report');
  expect(op.responseSchema.safeParse({ attemptId: 'attempt/d', report: null }).success).toBe(false);
  expect(op.responseSchema.parse({ attemptId: 'attempt/c', report: { kind: 'failed', reason: 'No result.' } }).report)
    .toEqual({ kind: 'failed', reason: 'No result.' });
});
