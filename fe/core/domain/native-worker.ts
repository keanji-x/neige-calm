import { z } from 'zod';
import type { ApiAbortSignal, ApiOperation } from '../api/types.js';
import type { TaskAttempt } from './task-recovery.js';

export type NativeWorkerRead = Readonly<{ flow: string; attempt: TaskAttempt | null; currentAttemptId: string | null }>;
/** Browser composition injects these operations; a Worker card owns no transport or terminal. */
export type NativeWorkerPort = Readonly<{
  read(track: string, key: string, card: string, attempt: string, signal?: ApiAbortSignal): Promise<NativeWorkerRead>;
  message(track: string, key: string, attempt: string, idempotencyKey: string, text: string): Promise<void>;
  cancel(track: string, key: string, attempt: string): Promise<void>;
}>;
export function workerFlowOperation(track: string, card: string, signal?: ApiAbortSignal): ApiOperation<{ content: string; content_type: string }> {
  return { method: 'GET', path: `/api/tracks/${encodeURIComponent(track)}/files/cat?path=${encodeURIComponent(`cards/${card}/conversation.md`)}`,
    responseSchema: z.object({ content: z.string(), content_type: z.string() }), ...(signal === undefined ? {} : { signal }) };
}
export function workerMessageOperation(track: string, key: string, attempt: string, idempotencyKey: string, text: string): ApiOperation<{ message_id: string }> {
  return { method: 'POST', path: `/api/tracks/${encodeURIComponent(track)}/tasks/${encodeURIComponent(key)}/messages`,
    body: { attempt_id: attempt, idempotency_key: idempotencyKey, text }, responseSchema: z.object({ message_id: z.string() }) };
}
export function workerCancelOperation(track: string, key: string, attempt: string): ApiOperation<{ ok: boolean }> {
  return { method: 'POST', path: `/api/tracks/${encodeURIComponent(track)}/tasks/${encodeURIComponent(key)}/cancel`,
    body: { attempt_id: attempt }, responseSchema: z.object({ ok: z.boolean() }) };
}
