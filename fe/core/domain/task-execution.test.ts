import { expect, it } from 'vitest';
import { currentTaskExecution } from './task-execution.js';
import type { TaskAttempt, TaskRecoveryView } from './task-recovery.js';

function view(id: string, generation: number): TaskRecoveryView & { current: TaskAttempt } {
  const current = { attempt_id: id, generation, status: 'failed', blocking_reason: null, status_detail: 'validation failed',
    worker_card_id: `${id}-worker`, created_at_ms: 1000, finished_at_ms: 2000 };
  return { key: 'b', current, attempts: [current] };
}

it('presents the current attempt of the history, and nothing without one', () => {
  expect(currentTaskExecution(undefined)).toBeUndefined();
  expect(currentTaskExecution({ key: 'b', current: null, attempts: [] })).toBeUndefined();
  expect(currentTaskExecution(view('later', 2))).toEqual({ attemptId: 'later', generation: 2, status: 'failed',
    label: 'Failed', statusDetail: 'validation failed', workerCardId: 'later-worker', blockingReason: null });
});

it('uses only the blocker belonging to the current attempt', () => {
  const previous = view('old', 1);
  const replacement = view('new', 2);
  replacement.attempts.unshift({ ...previous.current, blocking_reason: 'Old prerequisite missing.' });
  replacement.current = { ...replacement.current, status: 'awaiting_projection', blocking_reason: 'Release was withdrawn.' };
  expect(currentTaskExecution(replacement)?.blockingReason).toBe('Release was withdrawn.');
});
