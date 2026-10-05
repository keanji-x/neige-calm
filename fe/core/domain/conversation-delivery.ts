import type { ApiFailure } from '../api/types.js';
import { classifyFailure, type FailureTable } from './failure-class.js';

/** What one failed attempt of a keyed send says about whether its message was stored. */
export type SendFailureKind = 'unknown' | 'refused' | 'rejected';

/**
 * What one failed attempt of `POST /planner/input` says, taken alone. `rejected`: answered before
 * the request was handled, for a reason that can pass (429, 401), so the same send may be tried again.
 * `refused`: a refusal decided before any write, so the text is unspent, and the same body would be
 * refused again: the body itself (400, 413, 422), the card (403, 404) or the codes below (#2068).
 * `idempotency_key_reused` is one: the key is bound to another message, so no attempt under it can be
 * stored or replayed. The generic `conflict` is deliberately not one, the write may already have been
 * persisted. Anything else, `null` included (a connection that is not ready), is `unknown`;
 * `idempotency_key_concurrent` among them (another request under the key was stored at that moment,
 * so a retry under it replays that answer).
 * `planner_turn_not_replaceable` is an Edit's replace refused before anything was written (#2043).
 * `planner_harness_dormant`, `planner_harness_runtime_superseded` and `planner_turn_not_replaceable`
 * are a refusal only for an op with no unknown attempt yet: once one was unknown, a write of it may
 * still be queued and commit later while those codes come back, so {@link retryUnknownSend} keeps
 * such an op unknown.
 */
export const SEND_FAILURES: FailureTable<SendFailureKind> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ code: 'planner_harness_runtime_superseded', is: 'refused' as const }),
    Object.freeze({ code: 'planner_harness_dormant', is: 'refused' as const }),
    Object.freeze({ code: 'planner_turn_not_replaceable', is: 'refused' as const }),
    Object.freeze({ code: 'idempotency_key_reused', is: 'refused' as const }),
    Object.freeze({ status: Object.freeze([400, 403, 404, 413, 422]), is: 'refused' as const }),
    Object.freeze({ status: Object.freeze([429]), is: 'rejected' as const }),
  ]),
  unauthorized: 'rejected',
  otherwise: 'unknown',
});

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

/** A keyed send's answer, and whether the op was ever unknown before it came. */
export type KeyedSendAnswer<T> = Readonly<{ sent: T; everUnknown: boolean }>;

/**
 * Run `attempt` until it answers, fails for a reason other than an unknown outcome, or the retries
 * are spent; rejects with a {@link KeyedSendFailure}. Every attempt must reuse one
 * `Idempotency-Key`. `failureOf` reads an attempt's error as the {@link ApiFailure} it carries, or
 * `null`, which {@link SEND_FAILURES} classifies; `pause` waits before retry `retry`. `unknown` is
 * the op's state from earlier runs: a resumed op that was unknown ends unknown on anything but a
 * 200. A 200 after an unknown outcome may replay a message that has since been deleted, rewound
 * or reset, so `everUnknown` tells the caller not to trust its entry for display.
 */
export async function retryUnknownSend<T>(
  attempt: (index: number) => Promise<T>,
  failureOf: (error: unknown) => ApiFailure | null,
  pause: (retry: number) => Promise<void>,
  unknown: boolean,
): Promise<KeyedSendAnswer<T>> {
  let unknownSoFar = unknown;
  for (let index = 0; ; index += 1) {
    try {
      return { sent: await attempt(index), everUnknown: unknownSoFar };
    } catch (error) {
      const kind = classifyFailure(failureOf(error), SEND_FAILURES);
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
