import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';

/** Gate-free summaries from the task recovery service. Nullable fields are required. */
export const taskAttemptSchema = z.object({
  attempt_id: z.string().min(1),
  generation: z.number().int().positive(),
  status: z.string().min(1),
  status_detail: z.string().nullable(),
  blocking_reason: z.string().nullable(),
  worker_card_id: z.string().nullable(),
  created_at_ms: z.number(),
  finished_at_ms: z.number().nullable(),
});
export const taskRecoveryViewSchema = z.object({
  key: z.string().min(1),
  current: taskAttemptSchema,
  attempts: z.array(taskAttemptSchema).min(1),
  recovery: z.object({ allowed: z.boolean(), code: z.string(), reason: z.string() }),
});
export const taskRecoveryReceiptSchema = z.object({
  key: z.string(), previous_attempt_id: z.string().min(1),
  attempt_id: z.string().min(1), generation: z.number().int().positive(),
});
export type TaskAttempt = z.infer<typeof taskAttemptSchema>;
export type TaskRecoveryView = z.infer<typeof taskRecoveryViewSchema>;
export type TaskRecoveryReceipt = z.infer<typeof taskRecoveryReceiptSchema>;
export type TaskRecoveryRequest = Readonly<{
  expected_attempt_id: string;
  idempotency_key: string;
  reason: string;
}>;

export function taskAttemptsOperation(trackId: string, key: string): ApiOperation<TaskRecoveryView> {
  return { method: 'GET', path: `/api/tracks/${encodeURIComponent(trackId)}/tasks/${encodeURIComponent(key)}/attempts`,
    responseSchema: taskRecoveryViewSchema.refine((view) => view.key === key,
      { message: 'Task history belongs to a different task' }),
  };
}

/** The caller freezes this body once per user intent, including transport retries. */
export function recoverTaskOperation(trackId: string, key: string, request: TaskRecoveryRequest): ApiOperation<TaskRecoveryReceipt> {
  return { method: 'POST', path: `/api/tracks/${encodeURIComponent(trackId)}/tasks/${encodeURIComponent(key)}/recover`,
    body: request,
    responseSchema: taskRecoveryReceiptSchema.refine((receipt) => receipt.key === key
      && receipt.previous_attempt_id === request.expected_attempt_id,
    { message: 'Recovery receipt does not match the request' }),
  };
}

/** Dispatch is preparation; only running means business execution has begun. */
export function attemptStatusLabel(status: string): string {
  switch (status) {
    case 'awaiting_projection': return 'Waiting to start';
    case 'pending': return 'Queued';
    case 'dispatched': return 'Preparing';
    case 'running': return 'Running';
    case 'verifying': return 'Checking result';
    case 'done': return 'Completed';
    case 'failed': return 'Failed';
    case 'canceled': return 'Canceled';
    default: return status;
  }
}
