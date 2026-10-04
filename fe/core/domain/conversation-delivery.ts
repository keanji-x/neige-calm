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

/** `null` is a failure with no answer at all, such as a connection that is not ready. */
export function sendFailureKind(failure: ApiFailure | null): SendFailureKind {
  /* The server gives `planner_harness_dormant` and `planner_harness_runtime_superseded` only to a send it is about to
     store fresh: it replays a bound key first (crates/calm-server/src/routes/planner_input_send.rs:151) and reaches
     the harness lookup (:159) and the snapshot write (crates/calm-server/src/harness/run_loop.rs:3755) only after.
     So either answer proves that no attempt under the key was stored. */
  if (isSendRefusalCode(failure?.kind === 'http' ? failure.code : null)) return 'refused';
  return failedConversationDelivery(failure) === 'rejected' ? 'rejected' : 'unknown';
}

/** Automatic retries of one keyed send after its first attempt; an attempt that cannot go out counts too. */
export const SEND_RETRIES = 5;

/**
 * A keyed send that gave up. `cause` is the last attempt's error. `delivery` stays `unknown` once
 * any attempt's outcome was: an answered rejection after it (a 401, 403 or 429) says nothing
 * about whether that earlier attempt was stored, so only a pre-binding refusal clears it.
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
 * `Idempotency-Key`; `pause` waits before retry `retry`.
 */
export async function retryUnknownSend<T>(
  attempt: (index: number) => Promise<T>,
  classify: (error: unknown) => SendFailureKind,
  pause: (retry: number) => Promise<void>,
): Promise<T> {
  let unknownBefore = false;
  for (let index = 0; ; index += 1) {
    try {
      return await attempt(index);
    } catch (error) {
      const kind = classify(error);
      if (kind === 'unknown' && index < SEND_RETRIES) {
        unknownBefore = true;
        await pause(index);
        continue;
      }
      throw new KeyedSendFailure(error, kind === 'refused' || !unknownBefore ? kind : 'unknown');
    }
  }
}
