import { createUnauthorizedChannel, type MicrotaskSchedulerPort } from '../../../../core/api/unauthorized.ts';
import type { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
/** Fence both request completion and deferred delivery to the current identity owner. */
export function createRecoveryUnauthorizedChannel(access: RecoveryAccess, scheduler: MicrotaskSchedulerPort) {
  return createUnauthorizedChannel({ enqueue: (task) => {
    const generation = access.read().generation;
    scheduler.enqueue(() => { if (generation === access.read().generation) task(); });
  } });
}
