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

/**
 * A newly observed exact user message is a candidate for the reader to review,
 * never proof that this request arrived. The cached high-water can be stale,
 * and neither the item's sequence nor timestamp identifies this attempt.
 *
 * #1505 S6 review — an image with no words has to be matchable too.
 *
 * The text criterion refuses an empty echo, and rightly: two blank strings are
 * not evidence. But an image-only message IS a blank string, so this answered
 * `false` for it unconditionally — meaning the one message shape this slice
 * exists to add could never be reported as "we may already have it", and the
 * reader was offered a resend with no warning about a duplicate. The
 * attachment ids are the second criterion for exactly the reason they are one
 * in the echo reconciler: they are server-minted, one per upload, so a
 * persisted row carrying them IS the row that carried that image.
 */
export function hasUnseenMatchingConversationMessage(
  serverTurns: readonly ConversationMessage[], echo: OptimisticConversationTurn,
): boolean {
  const text = echo.text.trim();
  const echoAttachments = echo.attachments ?? [];
  if (text === '' && echoAttachments.length === 0) return false;
  return serverTurns.some((turn) => {
    if (turn.author !== 'you') return false;
    if (Number.parseInt(turn.id.split(':', 1)[0] ?? '', 10) <= echo.serverHighWaterBefore) {
      return false;
    }
    if (text !== '') return turn.text.trim() === text;
    const rowIds = new Set((turn.attachments ?? []).map((attachment) => attachment.id));
    return echoAttachments.every((attachment) => rowIds.has(attachment.id));
  });
}
