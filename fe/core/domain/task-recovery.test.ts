import { expect, it } from 'vitest';
import { performApiRequest } from '../api/client.js';
import { attemptStatusLabel, recoverTaskOperation, taskAttemptsOperation, taskRecoveryViewSchema } from './task-recovery.js';

function view() {
  const current = { attempt_id: 'server-id', generation: 2, status: 'awaiting_projection', status_detail: null,
    worker_card_id: null, created_at_ms: 1000, finished_at_ms: null };
  return { key: 'B', current, attempts: [current], recovery: { allowed: false, code: 'not_failed', reason: 'Waiting for admission.' } };
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
    taskAttemptsOperation('track/1', 'B'));
  expect(result).toMatchObject({ status: 'failed', error: { kind: 'decode' } });
});

it('requires recovery receipts to acknowledge the exact expected attempt', async () => {
  const request = { expected_attempt_id: 'server-id', idempotency_key: 'stable-intent', reason: 'Recover task' };
  const operation = recoverTaskOperation('track/1', 'B + C', request);
  expect(operation.path).toBe('/api/tracks/track%2F1/tasks/B%20%2B%20C/recover');
  const result = await performApiRequest({ send: () => Promise.resolve({ status: 200, statusText: 'OK', body: {
    key: 'B + C', previous_attempt_id: 'different-execution', attempt_id: 'new', generation: 2,
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
