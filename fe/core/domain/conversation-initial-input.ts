import { z } from 'zod';
import type { ConversationInitialInputReceipt, ConversationInitialInputReceiptResult, ConversationInitialInputObservation } from '../api/generated/wire.js';
import type { ApiOperation } from '../api/types.js';
import type { Conversation, ConversationTurn, PendingQueueEntry } from './conversation.js';
import type { LandedReads } from './conversation-outbox.js';
import { sha256Hex } from './sha256.js';

export type SubmittedConversationInput = Readonly<{ text: string; atMs: number }>;
export type CreatedConversation = Readonly<{ conversation: Conversation; initialInput: ConversationInitialInputReceiptResult }>;

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

/** A creation owns only its immutable original acceptance. It has no ordinary-input retry/edit/dismiss. */
export type AcceptedInitialInput = Readonly<{
  phase: 'initial-accepted'; key: string; trackId: string; submitted: SubmittedConversationInput;
  receipt: ConversationInitialInputReceipt;
}>;
export type ObservedInitialInput = Readonly<{ observation: ConversationInitialInputObservation; afterRead: number }>;

export function initialInputEcho(op: AcceptedInitialInput): ConversationTurn {
  return { id: `initial-${op.key}`, author: 'you', text: op.submitted.text, atMs: op.submitted.atMs };
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

/** Coherent queued state outranks stale projections left after rebuffer. Empty GET pairs cannot retire words. */
export function initialInputDisplaced(op: AcceptedInitialInput, observed: ObservedInitialInput | undefined): boolean {
  const observation = observed?.observation;
  return observation?.kind === 'absent' || (observation?.kind === 'queued' && observation.textSha256 !== op.receipt.textSha256);
}

/** A coherent observation fences old caches; later actual source reads may report a changed or restored input. */
export function initialInputSourcesFresh(observed: ObservedInitialInput, landed: LandedReads): boolean {
  return landed.transcript > observed.afterRead && landed.run > observed.afterRead;
}

/** Hide only stale cached queue ownership; a later actual run read is allowed to report a restored entry. */
export function initialInputQueueView(entries: readonly PendingQueueEntry[], op: AcceptedInitialInput | undefined,
  observed: ObservedInitialInput | undefined, runRead: number): Readonly<{ entries: readonly PendingQueueEntry[]; hiddenIds: ReadonlySet<string> }> {
  const hiddenIds = new Set<string>();
  const observation = observed?.observation;
  if (op === undefined || observed === undefined || observation === undefined || observation.kind === 'unavailable' || runRead > observed.afterRead) {
    return { entries, hiddenIds };
  }
  const shown = entries.filter((entry) => {
    if (entry.entry_id !== op.receipt.entryId) return true;
    const hide = observation.kind !== 'queued' || entry.rev < observation.rev || sha256Hex(entry.text) !== observation.textSha256;
    if (hide) hiddenIds.add(entry.entry_id);
    return !hide;
  });
  return { entries: hiddenIds.size === 0 ? entries : shown, hiddenIds };
}

export function initialInputRetirement(op: AcceptedInitialInput, observed: ObservedInitialInput | undefined,
  carrier: ConversationTurn | undefined, landed: LandedReads, hiddenQueue: boolean): { row: string | null } | null {
  if (observed === undefined) return null;
  const { observation } = observed;
  const freshSources = initialInputSourcesFresh(observed, landed);
  if (initialInputDisplaced(op, observed)) return freshSources ? { row: null } : null;
  if (observation.kind === 'projected') {
    // The caller resolves this carrier through the inspector's exact source pair.
    if (!hiddenQueue && carrier !== undefined) return { row: carrier.id };
    if (freshSources) return { row: null };
  }
  return null;
}
