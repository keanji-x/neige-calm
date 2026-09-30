import { expect, expectTypeOf, it } from 'vitest';
import type { TaskAttemptView as WireTaskAttempt, TaskRecoveryView as WireTaskRecoveryView } from '../api/generated/wire.js';
import type { TaskAttempt, TaskRecoveryView } from './task-recovery.js';
import { performApiRequest } from '../api/client.js';
import { attemptStatusLabel, taskAttemptsOperation, taskRecoveryViewSchema } from './task-recovery.js';

function view() {
  const current = { attempt_id: 'server-id', generation: 2, status: 'awaiting_projection', blocking_reason: null, status_detail: null,
    worker_card_id: null, created_at_ms: 1000, finished_at_ms: null };
  return { key: 'b', current, attempts: [current] };
}

it('requires nullable execution evidence keys and keeps an allocation without a projection', () => {
  expect(taskRecoveryViewSchema.parse(view()).current?.status).toBe('awaiting_projection');
  for (const field of ['blocking_reason', 'status_detail', 'worker_card_id', 'finished_at_ms']) {
    const body = view();
    Reflect.deleteProperty(body.current, field);
    expect(taskRecoveryViewSchema.safeParse(body).success).toBe(false);
  }
  const historical = view();
  historical.attempts = [{ ...historical.current }];
  Reflect.deleteProperty(historical.attempts[0], 'blocking_reason');
  expect(taskRecoveryViewSchema.safeParse(historical).success).toBe(false);
});

it('rejects history for another business task through the API boundary', async () => {
  const result = await performApiRequest({ send: () => Promise.resolve({ status: 200, statusText: 'OK', body: { ...view(), key: 'other' } }) },
    taskAttemptsOperation('w1', 'b'));
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
  expect(taskAttemptsOperation('track/1', 'b + c').path).toBe('/api/tracks/track%2F1/tasks/b%20%2B%20c/attempts');
});

it('matches the generated history contracts, including required nullable evidence', () => {
  expectTypeOf<TaskAttempt>().toEqualTypeOf<WireTaskAttempt>();
  expectTypeOf<TaskRecoveryView>().toEqualTypeOf<WireTaskRecoveryView>();
});

it('accepts explicit empty history and rejects inconsistent allocation evidence', () => {
  const empty = { key: 'b', current: null, attempts: [] };
  expect(taskRecoveryViewSchema.safeParse(empty).success).toBe(true);
  const missing = { ...empty };
  Reflect.deleteProperty(missing, 'current');
  expect(taskRecoveryViewSchema.safeParse(missing).success).toBe(false);
  expect(taskRecoveryViewSchema.safeParse({ ...empty, attempts: view().attempts }).success).toBe(false);
  expect(taskRecoveryViewSchema.safeParse({ ...view(), attempts: [] }).success).toBe(false);
});

it('history without recovery decodes, and a recovery field is not part of the contract', () => {
  const decoded = taskRecoveryViewSchema.parse({ ...view(), recovery: { allowed: true, code: 'available', reason: 'Recover.' } });
  expect(Object.keys(decoded).sort()).toEqual(['attempts', 'current', 'key']);
});
