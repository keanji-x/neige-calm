import type { ApiFailure } from '../api/types.js';
import { isSendRefusalCode } from './conversation.js';

/** These explicit request rejections happen before dispatch; every other outcome requires checking delivery. */
export function failedConversationDelivery(failure: ApiFailure | null): 'rejected' | 'unknown' {
  return failure !== null && (failure.kind === 'unauthorized'
    || (failure.kind === 'http' && [400, 403, 404, 413, 422, 429].includes(failure.status)))
    ? 'rejected' : 'unknown';
}

/** What one failed attempt of a keyed send says about whether its message was stored. */
export type SendFailureKind = 'unknown' | 'refused' | 'rejected';

/**
 * What one failed attempt says, taken alone. `null` is a failure with no answer at all, such as a
 * connection that is not ready. `planner_harness_dormant` and `planner_harness_runtime_superseded`
 * are a refusal only for an op with no unknown attempt yet: once one was unknown, a write of it may
 * still be queued and commit later while those codes come back, so {@link retryUnknownSend} keeps
 * such an op unknown.
 */
export function sendFailureKind(failure: ApiFailure | null): SendFailureKind {
  if (isSendRefusalCode(failure?.kind === 'http' ? failure.code : null)) return 'refused';
  return failedConversationDelivery(failure) === 'rejected' ? 'rejected' : 'unknown';
}

/** Automatic retries of one keyed send after its first attempt; an attempt that cannot go out counts too. */
export const SEND_RETRIES = 5;

/**
 * A keyed send that gave up. `cause` is the last attempt's error. `delivery` stays `unknown` once
 * any attempt's outcome was: no later answer short of a 200 says whether that attempt was stored.
 */
export class KeyedSendFailure extends Error {
  readonly delivery: SendFailureKind;

  constructor(cause: unknown, delivery: SendFailureKind) {
    super(cause instanceof Error ? cause.message : 'Could not send the message.', { cause });
    this.name = 'KeyedSendFailure';
    this.delivery = delivery;
  }
}

/**
 * Run `attempt` until it answers, fails for a reason other than an unknown outcome, or the retries
 * are spent; rejects with a {@link KeyedSendFailure}. Every attempt must reuse one
 * `Idempotency-Key`; `pause` waits before retry `retry`. `unknown` is the op's state from earlier
 * runs: a resumed op that was unknown ends unknown on anything but a 200.
 */
export async function retryUnknownSend<T>(
  attempt: (index: number) => Promise<T>,
  classify: (error: unknown) => SendFailureKind,
  pause: (retry: number) => Promise<void>,
  unknown: boolean,
): Promise<T> {
  let unknownSoFar = unknown;
  for (let index = 0; ; index += 1) {
    try {
      return await attempt(index);
    } catch (error) {
      const kind = classify(error);
      if (kind === 'unknown') {
        unknownSoFar = true;
        if (index < SEND_RETRIES) {
          await pause(index);
          continue;
        }
      }
      throw new KeyedSendFailure(error, unknownSoFar ? 'unknown' : kind);
    }
  }
}
