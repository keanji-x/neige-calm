// Read receipts (#2596): the kernel keeps what the one user has read, so a refresh or a second
// device reads the same state. This module is the one unread rule and the receipt operations.

import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';
import type {
  ReadReceipt as ReadReceiptWire, ReadReceiptKind as ReadReceiptKindWire, ReadReceipts as ReadReceiptsWire,
} from '../api/generated/wire.js';

export type ReadReceiptKind = ReadReceiptKindWire;
export type ReadReceipt = ReadReceiptWire;

export const readReceiptSchema: z.ZodType<ReadReceipt> = z.object({
  kind: z.enum(['track', 'conversation']),
  targetId: z.string(),
  readThroughMs: z.number().int().nonnegative(),
  manualUnread: z.boolean(),
});

export const readReceiptsSchema: z.ZodType<ReadReceiptsWire> = z.object({
  baselineMs: z.number().int(),
  receipts: z.array(readReceiptSchema),
});

/**
 * The reader's view of the kernel's receipts: the cutover baseline, and each receipt under
 * {@link receiptKey}. Immutable: every change returns a new state.
 */
export type ReadState = Readonly<{ baselineMs: number; receipts: Readonly<Record<string, ReadReceipt>> }>;

export function receiptKey(kind: ReadReceiptKind, targetId: string): string {
  return `${kind}:${targetId}`;
}

export function toReadState(wire: ReadReceiptsWire): ReadState {
  const receipts: Record<string, ReadReceipt> = {};
  for (const receipt of wire.receipts) receipts[receiptKey(receipt.kind, receipt.targetId)] = receipt;
  return { baselineMs: wire.baselineMs, receipts };
}

/**
 * The unread rule: marked unread by hand, or activity after both the baseline and what was read.
 * `activityAt` is the target's completion high-water mark (a track's `activityAt`, a conversation's
 * `lastTurnCompletedAt`); `null` means nothing has completed, which is never unread by itself.
 */
export function isUnread(state: ReadState, kind: ReadReceiptKind, targetId: string, activityAt: number | null): boolean {
  const receipt = state.receipts[receiptKey(kind, targetId)];
  if (receipt?.manualUnread === true) return true;
  return activityAt !== null && activityAt > Math.max(state.baselineMs, receipt?.readThroughMs ?? 0);
}

/** The state after a mark read, as the kernel stores it: the larger instant, and no manual unread. */
export function withMarkedRead(state: ReadState, kind: ReadReceiptKind, targetId: string, readThroughMs: number): ReadState {
  const previous = state.receipts[receiptKey(kind, targetId)];
  return withReceipt(state, {
    kind, targetId, manualUnread: false,
    readThroughMs: Math.max(previous?.readThroughMs ?? 0, readThroughMs),
  });
}

/** The state after a mark unread, as the kernel stores it: the instant read through is kept. */
export function withMarkedUnread(state: ReadState, kind: ReadReceiptKind, targetId: string): ReadState {
  const previous = state.receipts[receiptKey(kind, targetId)];
  return withReceipt(state, { kind, targetId, manualUnread: true, readThroughMs: previous?.readThroughMs ?? 0 });
}

/** The state with one receipt replaced, such as the one a write answered. */
export function withReceipt(state: ReadState, receipt: ReadReceipt): ReadState {
  return { ...state, receipts: { ...state.receipts, [receiptKey(receipt.kind, receipt.targetId)]: receipt } };
}

export function readReceiptsOperation(): ApiOperation<ReadReceiptsWire> {
  return { method: 'GET', path: '/api/read-receipts', responseSchema: readReceiptsSchema };
}

const receiptPath = (kind: ReadReceiptKind, targetId: string) =>
  `/api/read-receipts/${kind}/${encodeURIComponent(targetId)}`;

/** Idempotent; the kernel keeps the larger instant and answers the stored receipt. */
export function markReadOperation(kind: ReadReceiptKind, targetId: string, readThroughMs: number): ApiOperation<ReadReceipt> {
  return { method: 'POST', path: `${receiptPath(kind, targetId)}/read`, body: { readThroughMs }, responseSchema: readReceiptSchema };
}

/** Idempotent; the next mark read clears it. */
export function markUnreadOperation(kind: ReadReceiptKind, targetId: string): ApiOperation<ReadReceipt> {
  return { method: 'POST', path: `${receiptPath(kind, targetId)}/unread`, responseSchema: readReceiptSchema };
}
