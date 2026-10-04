import type { ApiFailure } from '../api/types.js';
import { isSendRefusalCode } from './conversation.js';

/** These explicit request rejections happen before dispatch; every other outcome requires checking delivery. */
export function failedConversationDelivery(failure: ApiFailure | null): 'rejected' | 'unknown' {
  return failure !== null && (failure.kind === 'unauthorized'
    || (failure.kind === 'http' && [400, 403, 404, 413, 422, 429].includes(failure.status)))
    ? 'rejected' : 'unknown';
}

/**
 * Whether a keyed send's failure leaves it unknown whether the message was stored, so the same key
 * is sent again. `null` is a failure with no answer at all, such as a connection that is not ready.
 */
export function isUnknownSendFailure(failure: ApiFailure | null): boolean {
  return failedConversationDelivery(failure) === 'unknown'
    && !isSendRefusalCode(failure?.kind === 'http' ? failure.code : null);
}

/** Automatic retries of one keyed send after its first attempt; an attempt that cannot go out counts too. */
export const SEND_RETRIES = 5;

/**
 * Run `attempt` until it answers, fails for a reason other than an unknown outcome, or the retries
 * are spent. Every attempt must reuse one `Idempotency-Key`; `pause` waits before retry `retry`.
 */
export async function retryUnknownSend<T>(
  attempt: (index: number) => Promise<T>,
  isUnknown: (error: unknown) => boolean,
  pause: (retry: number) => Promise<void>,
): Promise<T> {
  for (let index = 0; ; index += 1) {
    try {
      return await attempt(index);
    } catch (error) {
      if (index >= SEND_RETRIES || !isUnknown(error)) throw error;
      await pause(index);
    }
  }
}
