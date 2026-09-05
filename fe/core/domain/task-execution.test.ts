import { expect, it } from 'vitest';
import { currentTaskExecution } from './task-execution.js';
import type { TaskRecoveryView } from './task-recovery.js';

function view(id: string, generation: number): TaskRecoveryView {
  const current = { attempt_id: id, generation, status: 'failed', status_detail: 'validation failed',
    worker_card_id: `${id}-worker`, created_at_ms: 1000, finished_at_ms: 2000 };
  return { key: 'B', current, attempts: [current], recovery: { allowed: true, code: 'available', reason: 'Recover.' } };
}

it('fences every older or same-generation mismatched snapshot behind an accepted receipt', () => {
  const intent = { phase: 'accepted', receipt: { key: 'B', previous_attempt_id: 'old', attempt_id: 'new', generation: 2 } } as const;
  for (const snapshot of [undefined, view('old', 1), view('other', 2)]) {
    expect(currentTaskExecution(snapshot, intent)).toEqual({ attemptId: 'new', generation: 2,
      status: 'awaiting_refresh', label: 'Awaiting execution refresh', statusDetail: null, workerCardId: null });
  }
  expect(currentTaskExecution(view('new', 2), intent)).toMatchObject({ attemptId: 'new', status: 'failed', workerCardId: 'new-worker' });
  expect(currentTaskExecution(view('later', 3), intent)).toMatchObject({ attemptId: 'later', generation: 3, workerCardId: 'later-worker' });
});

it('does not expose a predecessor as current while the recovery response is uncertain', () => {
  const snapshot = view('old', 1);
  const request = { expected_attempt_id: 'old', idempotency_key: 'one-intent', reason: 'Recover.' };
  expect(currentTaskExecution(snapshot, { phase: 'uncertain', request })).toEqual({ attemptId: null, generation: null,
    status: 'awaiting_refresh', label: 'Awaiting recovery confirmation', statusDetail: null, workerCardId: null });
  const newer = view('next', 2);
  newer.attempts.unshift(snapshot.current);
  expect(currentTaskExecution(newer, { phase: 'uncertain', request })).toMatchObject({ attemptId: 'next', generation: 2 });
});
