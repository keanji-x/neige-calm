import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';

/** Gate-free summaries of a task's execution history. Nullable fields are required. */
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
  current: taskAttemptSchema.nullable(),
  attempts: z.array(taskAttemptSchema),
}).refine((view) => {
  if (view.current === null) return view.attempts.length === 0;
  const latest = view.attempts.at(-1);
  return latest?.attempt_id === view.current.attempt_id && latest.generation === view.current.generation;
}, { message: 'Task history has inconsistent current allocation evidence' });
export type TaskAttempt = z.infer<typeof taskAttemptSchema>;
export type TaskRecoveryView = z.infer<typeof taskRecoveryViewSchema>;

export function taskAttemptsOperation(trackId: string, key: string): ApiOperation<TaskRecoveryView> {
  return { method: 'GET', path: `/api/tracks/${encodeURIComponent(trackId)}/tasks/${encodeURIComponent(key)}/attempts`,
    responseSchema: taskRecoveryViewSchema.refine((view) => view.key === key,
      { message: 'Task history belongs to a different task' }),
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
