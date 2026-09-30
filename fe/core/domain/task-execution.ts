import { attemptStatusLabel, type TaskRecoveryView } from './task-recovery.js';

/** One presentation of known current identity, shared by inventory, header and detail. */
export type CurrentTaskExecution = Readonly<{
  attemptId: string;
  generation: number;
  status: string;
  label: string;
  statusDetail: string | null;
  workerCardId: string | null;
  blockingReason: string | null;
}>;

export function currentTaskExecution(view: TaskRecoveryView | undefined): CurrentTaskExecution | undefined {
  const current = view?.current ?? undefined;
  if (current === undefined) return undefined;
  return { attemptId: current.attempt_id, generation: current.generation, status: current.status,
    label: attemptStatusLabel(current.status), statusDetail: current.status_detail, workerCardId: current.worker_card_id, blockingReason: current.blocking_reason };
}
