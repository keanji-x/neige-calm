import type { ApiRequest } from '../../../../core/api/types.ts';
import type { ConversationInitialInputReceipt } from '../../../../core/api/generated/wire.ts';
import { sha256Hex } from '../../../../core/domain/sha256.ts';

/** Explicit fake-kernel mint evidence from the actual dispatched request; no receipt is backfilled on decode. */
export function initialReceiptFor(request: ApiRequest, entryId: string, entryRev: number): ConversationInitialInputReceipt {
  if (typeof request.body !== 'object' || request.body === null || !('text' in request.body) || typeof request.body.text !== 'string') {
    throw new Error('The first-input producer requires the actual submitted text.');
  }
  return { version: 'initial-input-v1', entryId, entryRev, queuedAtMs: 100, textSha256: sha256Hex(request.body.text), initialRuntimeId: 'runtime' };
}
