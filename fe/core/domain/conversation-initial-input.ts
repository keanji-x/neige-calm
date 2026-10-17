import { z } from 'zod';
import type { ConversationInitialInputReceipt, ConversationInitialInputReceiptResult, ConversationInitialInputObservation } from '../api/generated/wire.js';
import type { ApiOperation } from '../api/types.js';
import { sha256Hex } from './sha256.js';

export type SubmittedConversationInput = Readonly<{ text: string; atMs: number }>;

export const initialInputReceiptSchema: z.ZodType<ConversationInitialInputReceipt> = z.object({
  version: z.literal('initial-input-v1'), entryId: z.string().min(1), queuedAtMs: z.number().int(),
  textSha256: z.string().regex(/^[0-9a-f]{64}$/), initialRuntimeId: z.string().min(1),
});
export const initialInputReceiptResultSchema: z.ZodType<ConversationInitialInputReceiptResult> = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('known'), receipt: initialInputReceiptSchema }),
  z.object({ kind: z.literal('legacy-unavailable') }),
]);

const initialInputObservationSchema: z.ZodType<ConversationInitialInputObservation> = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('queued'), receipt: initialInputReceiptSchema, runtimeId: z.string().min(1),
    entryId: z.string().min(1), rev: z.number().int().nonnegative(), text: z.string(), textSha256: z.string().regex(/^[0-9a-f]{64}$/) })
    .refine(value => value.entryId === value.receipt.entryId && sha256Hex(value.text) === value.textSha256),
  z.object({ kind: z.literal('projected'), receipt: initialInputReceiptSchema, runtimeId: z.string().min(1),
    itemId: z.number().int().positive(), segmentIndex: z.number().int().nonnegative() }),
  z.object({ kind: z.literal('absent'), receipt: initialInputReceiptSchema, runtimeId: z.string().min(1) }),
  z.object({ kind: z.literal('unavailable'), receipt: initialInputReceiptSchema.nullable(), reason: z.enum(['legacy-operation', 'creation-incomplete', 'runtime-unavailable',
    'runtime-changed', 'shutting-down', 'legacy-projection', 'integrity']) }).refine(value =>
      ['legacy-operation', 'creation-incomplete', 'integrity'].includes(value.reason) ? value.receipt === null : value.receipt !== null),
]);

export function initialInputObservationOperation(trackId: string, cardId: string, key: string): ApiOperation<ConversationInitialInputObservation> {
  return { method: 'GET', path: `/api/tracks/${encodeURIComponent(trackId)}/conversations/${encodeURIComponent(cardId)}/initial-input`,
    headers: { 'Idempotency-Key': key }, responseSchema: initialInputObservationSchema };
}

/** Validate the original body and every observation's immutable receipt at the consumer boundary. */
export function receiptMatchesSubmission(receipt: ConversationInitialInputReceipt, submitted: SubmittedConversationInput): boolean {
  return receipt.textSha256 === sha256Hex(submitted.text);
}
export function observationMatchesReceipt(observation: ConversationInitialInputObservation, receipt: ConversationInitialInputReceipt): boolean {
  if (observation.kind === 'unavailable' && observation.receipt === null) return true;
  const actual = observation.receipt;
  if (actual === null) return false;
  return actual.version === receipt.version && actual.entryId === receipt.entryId && actual.queuedAtMs === receipt.queuedAtMs
    && actual.textSha256 === receipt.textSha256 && actual.initialRuntimeId === receipt.initialRuntimeId
    && (observation.kind !== 'queued' || (observation.entryId === receipt.entryId && sha256Hex(observation.text) === observation.textSha256));
}
