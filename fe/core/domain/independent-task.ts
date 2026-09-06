import { z } from 'zod';
import type { ApiFailure, ApiOperation } from '../api/types.js';
import { isTerminal, type CardWire, type TrackLifecycle } from './track.js';
import { readTrackReport } from './report.js';

/** One immutable request per user intent, including uncertain retries. */
export type IndependentTaskRequest = Readonly<{ key: string; goal: string; ifDocRev: number }>;
export const independentTaskReceiptSchema = z.object({
  taskKey: z.string().min(1), blockId: z.string().min(1), docRev: z.number().int().nonnegative(),
});
export type IndependentTaskReceipt = z.infer<typeof independentTaskReceiptSchema>;
export const acceptedTaskReportSchema = z.object({
  attemptId: z.string().min(1),
  report: z.discriminatedUnion('kind', [
    z.object({ kind: z.literal('completed'), result: z.json(), artifacts: z.array(z.string()) }),
    z.object({ kind: z.literal('failed'), reason: z.string() }),
  ]).nullable(),
});
export type AcceptedTaskReport = z.infer<typeof acceptedTaskReportSchema>;
export type IndependentTaskIntent =
  | Readonly<{ phase: 'editing'; goal: string }>
  | Readonly<{ phase: 'sending' | 'uncertain'; request: IndependentTaskRequest; message: string | null }>
  | Readonly<{ phase: 'rejected'; request: IndependentTaskRequest; message: string }>
  | Readonly<{ phase: 'accepted'; request: IndependentTaskRequest; receipt: IndependentTaskReceipt; revealed: boolean }>;

export function startIndependentTaskOperation(trackId: string, request: IndependentTaskRequest): ApiOperation<IndependentTaskReceipt> {
  return { method: 'POST', path: `/api/tracks/${encodeURIComponent(trackId)}/isolated-tasks`, body: request,
    responseSchema: independentTaskReceiptSchema.refine((receipt) => receipt.taskKey === request.key,
      { message: 'Task receipt belongs to a different task' }),
  };
}

export function acceptedTaskReportOperation(trackId: string, key: string, attemptId: string): ApiOperation<AcceptedTaskReport> {
  return { method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/tasks/${encodeURIComponent(key)}/attempts/${encodeURIComponent(attemptId)}/report`,
    responseSchema: acceptedTaskReportSchema.refine((value) => value.attemptId === attemptId,
      { message: 'Report belongs to a different attempt' }),
  };
}

/** Fail closed for missing or malformed revisions; the viewer's empty state is not a revision. */
export function independentTaskRevision(cards: readonly CardWire[]): number | null {
  const report = cards.find((card) => card.kind === 'track-report');
  const parsed = z.object({ docRev: z.number().int().nonnegative() }).safeParse(report?.payload);
  return parsed.success ? parsed.data.docRev : null;
}

/** Reconcile only the exact authored key and goal; never overwrite another block. */
export function findIndependentTask(cards: readonly CardWire[], request: IndependentTaskRequest): IndependentTaskReceipt | null {
  const matches = readTrackReport(cards)?.blocks?.filter((block) => block.kind === 'task' && block.payload.key === request.key) ?? [];
  const block = matches.length === 1 ? matches[0] : undefined;
  const docRev = independentTaskRevision(cards);
  if (block?.kind !== 'task' || 'tombstoned_by' in block.payload || block.payload.kind !== 'codex'
    || block.payload.declared_by !== 'user' || block.payload.goal !== request.goal || docRev === null) return null;
  return { taskKey: request.key, blockId: block.id, docRev };
}

export function independentTaskFailureUncertain(failure: ApiFailure): boolean {
  return failure.kind === 'transport' || failure.kind === 'decode'
    || (failure.kind === 'http' && (failure.status >= 500 || failure.status === 408));
}

/** Mirrors the existing scheduler fence; Draft is started atomically by this endpoint. */
export function independentTaskUnavailableReason(lifecycle: TrackLifecycle): string | null {
  if (isTerminal(lifecycle)) return 'This Track has ended. Resume it before starting another task.';
  if (lifecycle === 'blocked') return 'This Track is blocked. Resolve the blocker before starting another task.';
  return null;
}
