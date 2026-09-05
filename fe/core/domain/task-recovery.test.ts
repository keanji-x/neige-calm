import { expect, expectTypeOf, it } from 'vitest';
import type { TaskAttemptView as WireTaskAttempt, TaskRecoveryCapability as WireTaskRecoveryCapability, TaskRecoveryView as WireTaskRecoveryView, TaskRecoveryReceipt as WireTaskRecoveryReceipt, TaskRecoveryRequest as WireTaskRecoveryRequest } from '../api/generated/wire.js';
import type { TaskAttempt, TaskRecoveryView, TaskRecoveryReceipt, TaskRecoveryRequest } from './task-recovery.js';
import { performApiRequest } from '../api/client.js';
import { attemptStatusLabel, recoverTaskOperation, taskAttemptsOperation, taskRecoveryViewSchema } from './task-recovery.js';

function view() {
  const current = { attempt_id: 'server-id', generation: 2, status: 'awaiting_projection', status_detail: null,
    worker_card_id: null, created_at_ms: 1000, finished_at_ms: null };
  return { key: 'b', current, attempts: [current], recovery: { allowed: false, code: 'not_failed', reason: 'Waiting for admission.' } };
}

it('requires nullable execution evidence keys and keeps an allocation without a projection', () => {
  expect(taskRecoveryViewSchema.parse(view()).current.status).toBe('awaiting_projection');
  for (const field of ['status_detail', 'worker_card_id', 'finished_at_ms']) {
    const body = view();
    Reflect.deleteProperty(body.current, field);
    expect(taskRecoveryViewSchema.safeParse(body).success).toBe(false);
  }
});

it('rejects history for another business task through the API boundary', async () => {
  const result = await performApiRequest({ send: () => Promise.resolve({ status: 200, statusText: 'OK', body: { ...view(), key: 'other' } }) },
    taskAttemptsOperation('w1', 'b'));
  expect(result).toMatchObject({ status: 'failed', error: { kind: 'decode' } });
});

it('requires recovery receipts to acknowledge the exact expected attempt', async () => {
  const request = { expected_attempt_id: 'server-id', idempotency_key: 'stable-intent', reason: 'Recover task' };
  const operation = recoverTaskOperation('w1', 'b', request);
  expect(operation.path).toBe('/api/tracks/w1/tasks/b/recover');
  const result = await performApiRequest({ send: () => Promise.resolve({ status: 200, statusText: 'OK', body: {
    key: 'b', previous_attempt_id: 'different-execution', attempt_id: 'new', generation: 2,
  } }) }, operation);
  expect(result).toMatchObject({ status: 'failed', error: { kind: 'decode' } });
});

it('keeps preparation labels distinct from running and drops undeclared private gate fields', () => {
  expect(attemptStatusLabel('pending')).toBe('Queued');
  expect(attemptStatusLabel('dispatched')).toBe('Preparing');
  expect(attemptStatusLabel('running')).toBe('Running');
  const body = view();
  const result = taskRecoveryViewSchema.parse({ ...body, current: { ...body.current, gate: { cmd: 'secret check' } } });
  expect(result.current).not.toHaveProperty('gate');
});


it('escapes invalid route segments without claiming that the server admits them', () => {
  // This only exercises transport encoding. The server rejects this task key.
  const operation = recoverTaskOperation('track/1', 'b + c', {
    expected_attempt_id: 'server-id', idempotency_key: 'encoding-only', reason: 'Encoding probe',
  });
  expect(operation.path).toBe('/api/tracks/track%2F1/tasks/b%20%2B%20c/recover');
});

it('matches all generated recovery contracts, including required nullable evidence', () => {
  expectTypeOf<TaskAttempt>().toEqualTypeOf<WireTaskAttempt>();
  expectTypeOf<TaskRecoveryView>().toEqualTypeOf<WireTaskRecoveryView>();
  expectTypeOf<TaskRecoveryView['recovery']>().toEqualTypeOf<WireTaskRecoveryCapability>();
  expectTypeOf<TaskRecoveryReceipt>().toEqualTypeOf<WireTaskRecoveryReceipt>();
  expectTypeOf<TaskRecoveryRequest>().toEqualTypeOf<Readonly<WireTaskRecoveryRequest>>();
});
