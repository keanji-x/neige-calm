import type { ApiFailure } from '../api/types.js';
import type { ConversationMessage, OptimisticConversationTurn } from './conversation.js';

/** These explicit request rejections happen before dispatch; every other outcome requires checking delivery. */
export function failedConversationDelivery(failure: ApiFailure | null): 'rejected' | 'unknown' {
  return failure !== null && (failure.kind === 'unauthorized'
    || (failure.kind === 'http' && [400, 403, 404, 413, 422, 429].includes(failure.status)))
    ? 'rejected' : 'unknown';
}

/**
 * A newly observed exact user message is a candidate for the reader to review, never proof
 * that this request arrived. Attachment ids are server-minted, one per upload, so they match
 * an image-only message whose text is blank.
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
