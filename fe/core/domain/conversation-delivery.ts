import type { ApiFailure } from '../api/types.js';
import type { ConversationMessage, OptimisticConversationTurn } from './conversation.js';

/** Server/gateway failures, conflicts, and missing/malformed acknowledgements
 * can follow acceptance. These explicit request rejections happen before
 * dispatch; every other outcome requires checking delivery or informed resend. */
export function failedConversationDelivery(failure: ApiFailure | null): 'rejected' | 'unknown' {
  return failure !== null && (failure.kind === 'unauthorized'
    || (failure.kind === 'http' && [400, 403, 404, 413, 422, 429].includes(failure.status)))
    ? 'rejected' : 'unknown';
}

/** A newly observed exact user message is a candidate for the reader to review,
 * never proof that this request arrived. The cached high-water can be stale,
 * and neither the item's sequence nor timestamp identifies this attempt. */
export function hasUnseenMatchingConversationMessage(
  serverTurns: readonly ConversationMessage[], echo: OptimisticConversationTurn,
): boolean {
  const text = echo.text.trim();
  return text !== '' && serverTurns.some((turn) => turn.author === 'you'
    && Number.parseInt(turn.id.split(':', 1)[0] ?? '', 10) > echo.serverHighWaterBefore
    && turn.text.trim() === text);
}
