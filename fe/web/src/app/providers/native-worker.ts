import type { QueryClient } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { workerCancelOperation, workerFlowOperation, workerMessageOperation, type NativeWorkerPort } from '../../../../core/domain/native-worker.ts';
import { taskAttemptsOperation } from '../../../../core/domain/task-recovery.ts';
import { queryKeys, runOperation } from './queries.ts';

export function createNativeWorkerPort(transport: ApiTransportPort, unauthorized: UnauthorizedChannel, client: QueryClient): NativeWorkerPort {
  const refresh = (track: string) => client.invalidateQueries({ queryKey: queryKeys.trackDetail(track) });
  return Object.freeze({
    read: async (track, key, card, attempt, signal) => {
      const [flow, history] = await Promise.all([
        runOperation(transport, workerFlowOperation(track, card, signal), unauthorized),
        runOperation(transport, taskAttemptsOperation(track, key, signal), unauthorized),
      ]);
      return { flow: flow.content, attempt: history.attempts.find(row => row.attempt_id === attempt) ?? null, currentAttemptId: history.current?.attempt_id ?? null };
    },
    message: async (track, key, attempt, idempotencyKey, text) => {
      await runOperation(transport, workerMessageOperation(track, key, attempt, idempotencyKey, text), unauthorized);
      await refresh(track);
    },
    cancel: async (track, key, attempt) => {
      await runOperation(transport, workerCancelOperation(track, key, attempt), unauthorized);
      await refresh(track);
    },
  });
}
