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

/** A failed send is confirmed only by its exact sentence in a newer persisted
 * user row. Unlike the display echo matcher, a prefix is insufficient evidence
 * to retire recovery work. The transcript converter trims user segments. */
export function confirmsConversationDelivery(
  serverTurns: readonly ConversationMessage[], echo: OptimisticConversationTurn,
): boolean {
  const text = echo.text.trim();
  return text !== '' && serverTurns.some((turn) => turn.author === 'you'
    && Number.parseInt(turn.id.split(':', 1)[0] ?? '', 10) > echo.serverHighWaterBefore
    && turn.text.trim() === text);
}
