import { describe, expect, expectTypeOf, it } from 'vitest';
import type { z } from 'zod';

import type { ReadReceipt as GeneratedReceipt, ReadReceipts as GeneratedReceipts } from '../api/generated/wire.js';
import {
  isUnread, markReadOperation, markUnreadOperation, readReceiptSchema, readReceiptsOperation, readReceiptsSchema, toReadState,
  withMarkedRead, withMarkedUnread, withReceipt, type ReadState,
} from './read-receipts.js';

const state = (baselineMs: number, ...receipts: GeneratedReceipt[]): ReadState => toReadState({ baselineMs, receipts });
const receipt = (kind: 'track' | 'conversation', targetId: string, readThroughMs: number, manualUnread = false): GeneratedReceipt =>
  ({ kind, targetId, readThroughMs, manualUnread });

describe('the unread rule', () => {
  it('is manual unread, or activity after both the baseline and the receipt', () => {
    const read = state(100, receipt('track', 'seen', 150), receipt('track', 'marked', 500, true));
    expect(isUnread(read, 'track', 'never', 100)).toBe(false);
    expect(isUnread(read, 'track', 'never', 101)).toBe(true);
    expect(isUnread(read, 'track', 'seen', 150)).toBe(false);
    expect(isUnread(read, 'track', 'seen', 151)).toBe(true);
    expect(isUnread(read, 'track', 'marked', 10)).toBe(true);
    expect(isUnread(read, 'track', 'marked', null)).toBe(true);
  });

  it('never lights a target with no completed activity, and keeps kinds apart', () => {
    const read = state(0, receipt('conversation', 'same-id', 200));
    expect(isUnread(read, 'track', 'anything', null)).toBe(false);
    expect(isUnread(read, 'conversation', 'same-id', 200)).toBe(false);
    expect(isUnread(read, 'track', 'same-id', 200)).toBe(true);
  });
});

describe('optimistic writes mirror the kernel', () => {
  it('mark read keeps the larger instant and clears a manual unread', () => {
    const read = state(0, receipt('track', 't', 300, true));
    expect(withMarkedRead(read, 'track', 't', 100).receipts['track:t']).toEqual(receipt('track', 't', 300));
    expect(withMarkedRead(read, 'track', 't', 400).receipts['track:t']).toEqual(receipt('track', 't', 400));
    expect(read.receipts['track:t']).toEqual(receipt('track', 't', 300, true));
  });

  it('mark unread keeps the instant read through, zero for a target never read', () => {
    const read = state(0, receipt('track', 't', 300));
    expect(withMarkedUnread(read, 'track', 't').receipts['track:t']).toEqual(receipt('track', 't', 300, true));
    expect(withMarkedUnread(read, 'conversation', 'c').receipts['conversation:c']).toEqual(receipt('conversation', 'c', 0, true));
  });

  it('a write answer replaces its receipt only', () => {
    const read = state(5, receipt('track', 'a', 1), receipt('track', 'b', 2));
    const next = withReceipt(read, receipt('track', 'a', 9, true));
    expect(next).toEqual(state(5, receipt('track', 'a', 9, true), receipt('track', 'b', 2)));
  });
});

describe('operations', () => {
  it('match the generated wire types', () => {
    expectTypeOf<z.infer<typeof readReceiptSchema>>().toEqualTypeOf<GeneratedReceipt>();
    expectTypeOf<z.infer<typeof readReceiptsSchema>>().toEqualTypeOf<GeneratedReceipts>();
  });

  it('address the kernel routes with an encoded id', () => {
    expect(readReceiptsOperation()).toMatchObject({ method: 'GET', path: '/api/read-receipts' });
    expect(markReadOperation('conversation', 'a/b', 42)).toMatchObject({
      method: 'POST', path: '/api/read-receipts/conversation/a%2Fb/read', body: { readThroughMs: 42 },
    });
    expect(markUnreadOperation('track', 't 1')).toMatchObject({ method: 'POST', path: '/api/read-receipts/track/t%201/unread' });
    expect(markUnreadOperation('track', 't').body).toBeUndefined();
  });

  it('refuse a receipt outside the closed kind set', () => {
    expect(readReceiptsSchema.safeParse({ baselineMs: 1, receipts: [receipt('track', 't', 1)] }).success).toBe(true);
    expect(readReceiptsSchema.safeParse({ baselineMs: 1, receipts: [{ ...receipt('track', 't', 1), kind: 'area' }] }).success).toBe(false);
    expect(readReceiptsSchema.safeParse({ receipts: [] }).success).toBe(false);
  });
});
