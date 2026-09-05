import { attemptStatusLabel, type TaskRecoveryView, type TaskRecoveryReceipt,
  type TaskRecoveryRequest } from './task-recovery.js';

export type TaskRecoveryIntent =
  | Readonly<{ phase: 'idle' }>
  | Readonly<{ phase: 'sending' | 'uncertain'; request: TaskRecoveryRequest }>
  | Readonly<{ phase: 'accepted'; receipt: TaskRecoveryReceipt }>
  | Readonly<{ phase: 'rejected'; message: string }>;

/** One presentation of known current identity, shared by inventory, header and detail. */
export type CurrentTaskExecution = Readonly<{
  attemptId: string | null;
  generation: number | null;
  status: string;
  label: string;
  statusDetail: string | null;
  workerCardId: string | null;
  blockingReason: string | null;
}>;

/** A receipt is an identity fence, not proof that its new worker has started. */
export function currentTaskExecution(
  view: TaskRecoveryView | undefined, intent: TaskRecoveryIntent | undefined,
): CurrentTaskExecution | undefined {
  const current = view?.current;
  if (intent?.phase === 'accepted' && (current === undefined
    || current.generation < intent.receipt.generation
    || (current.generation === intent.receipt.generation && current.attempt_id !== intent.receipt.attempt_id))) {
    return { attemptId: intent.receipt.attempt_id, generation: intent.receipt.generation,
      status: 'awaiting_refresh', label: 'Awaiting execution refresh', statusDetail: null, workerCardId: null, blockingReason: null };
  }
  if (intent?.phase === 'sending' || intent?.phase === 'uncertain') {
    const predecessor = view?.attempts.find((attempt) => attempt.attempt_id === intent.request.expected_attempt_id);
    if (current === undefined || predecessor === undefined || current.generation <= predecessor.generation) {
      return { attemptId: null, generation: null, status: 'awaiting_refresh',
        label: intent.phase === 'sending' ? 'Requesting recovery' : 'Awaiting recovery confirmation',
        statusDetail: null, workerCardId: null, blockingReason: null };
    }
  }
  if (current === undefined) return undefined;
  return { attemptId: current.attempt_id, generation: current.generation, status: current.status,
    label: attemptStatusLabel(current.status), statusDetail: current.status_detail, workerCardId: current.worker_card_id, blockingReason: current.blocking_reason };
}
