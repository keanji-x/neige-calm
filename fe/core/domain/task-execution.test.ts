import { expect, it } from 'vitest';
import { currentTaskExecution } from './task-execution.js';
import type { TaskAttempt, TaskRecoveryView } from './task-recovery.js';

function view(id: string, generation: number): TaskRecoveryView & { current: TaskAttempt } {
  const current = { attempt_id: id, generation, status: 'failed', blocking_reason: null, status_detail: 'validation failed',
    worker_card_id: `${id}-worker`, created_at_ms: 1000, finished_at_ms: 2000 };
  return { key: 'b', current, attempts: [current], recovery: { allowed: true, code: 'available', reason: 'Recover.' } };
}

it('fences every older or same-generation mismatched snapshot behind an accepted receipt', () => {
  const intent = { phase: 'accepted', receipt: { key: 'b', previous_attempt_id: 'old', attempt_id: 'new', generation: 2 } } as const;
  for (const snapshot of [undefined, view('old', 1), view('other', 2)]) {
    expect(currentTaskExecution(snapshot, intent)).toEqual({ attemptId: 'new', generation: 2,
      status: 'awaiting_refresh', label: 'Awaiting execution refresh', statusDetail: null, workerCardId: null, blockingReason: null });
  }
  expect(currentTaskExecution(view('new', 2), intent)).toMatchObject({ attemptId: 'new', status: 'failed', workerCardId: 'new-worker' });
  expect(currentTaskExecution(view('later', 3), intent)).toMatchObject({ attemptId: 'later', generation: 3, workerCardId: 'later-worker' });
});

it('does not expose a predecessor as current while the recovery response is uncertain', () => {
  const snapshot = view('old', 1);
  const request = { expected_attempt_id: 'old', idempotency_key: 'one-intent', reason: 'Recover.' };
  expect(currentTaskExecution(snapshot, { phase: 'uncertain', request })).toEqual({ attemptId: null, generation: null,
    status: 'awaiting_refresh', label: 'Awaiting recovery confirmation', statusDetail: null, workerCardId: null, blockingReason: null });
  const newer = view('next', 2);
  newer.attempts.unshift(snapshot.current);
  expect(currentTaskExecution(newer, { phase: 'uncertain', request })).toMatchObject({ attemptId: 'next', generation: 2 });
});


it('uses only the blocker belonging to the selected current attempt', () => {
  const previous = view('old', 1);
  previous.current = { ...previous.current, status: 'pending', blocking_reason: 'Old prerequisite missing.' };
  const intent = { phase: 'accepted', receipt: { key: 'b', previous_attempt_id: 'old', attempt_id: 'new', generation: 2 } } as const;
  expect(currentTaskExecution(previous, intent)?.blockingReason).toBeNull();
  const replacement = view('new', 2);
  replacement.attempts.unshift(previous.current);
  replacement.current = { ...replacement.current, status: 'awaiting_projection', blocking_reason: 'Release was withdrawn.' };
  expect(currentTaskExecution(replacement, intent)?.blockingReason).toBe('Release was withdrawn.');
  expect(currentTaskExecution({ ...replacement, current: { ...replacement.current, attempt_id: 'other' } }, intent)?.blockingReason).toBeNull();
});
