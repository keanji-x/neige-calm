import { describe, expect, it } from 'vitest';

import type { HarnessLiveReplies, HarnessPhaseTag } from '../api/generated/wire.js';
import {
  awaitsSettling, harnessLiveOperation, liveReplyTurns, reconcileLiveReplies, replyMayStream,
  type LiveReplyCopy, type LiveReplyObservation, type LiveReplyTranscriptRow,
} from './conversation-live.js';

const poll = (turnId: string | null, items: Record<string, string>, atMs = 1): Extract<LiveReplyObservation, { kind: 'poll' }> => ({
  kind: 'poll', atMs, activeTurnId: null,
  reply: { turn_id: turnId, items: Object.entries(items).map(([item_id, text]) => ({ item_id, text })) } satisfies HarnessLiveReplies,
});
const phase = (tag: HarnessPhaseTag, latestReadStart: number): LiveReplyObservation =>
  ({ kind: 'phase', phase: tag, latestReadStart });
const transcript = (readStart: number, items: readonly LiveReplyTranscriptRow[] = []): LiveReplyObservation =>
  ({ kind: 'transcript', items, readStart });

function completed(itemUuid: string, method = 'item/completed'): LiveReplyTranscriptRow {
  return { method, item_uuid: itemUuid };
}

function run(...observations: readonly LiveReplyObservation[]): readonly LiveReplyCopy[] {
  return observations.reduce<readonly LiveReplyCopy[]>(reconcileLiveReplies, []);
}

const texts = (copies: readonly LiveReplyCopy[]) => copies.map((copy) => `${copy.turnId}/${copy.itemId}:${copy.text}`);

describe('reconcileLiveReplies', () => {
  it('holds each item of the polled turn and grows its text', () => {
    expect(texts(run(poll('T', { x: 'Hel' }), poll('T', { x: 'Hello', y: 'Second' }))))
      .toEqual(['T/x:Hello', 'T/y:Second']);
  });

  it('never shortens a held text', () => {
    expect(texts(run(poll('T', { x: 'Hello' }), poll('T', { x: 'He' })))).toEqual(['T/x:Hello']);
  });

  it('does not retire a copy that a poll merely omits', () => {
    expect(texts(run(poll('T', { x: 'Hello' }), poll('T', {}), poll(null, {})))).toEqual(['T/x:Hello']);
  });

  it('answers the same array when an observation changes nothing', () => {
    const held = run(poll('T', { x: 'Hello' }));
    expect(reconcileLiveReplies(held, poll('T', { x: 'Hello' }))).toBe(held);
    expect(reconcileLiveReplies(held, transcript(5))).toBe(held);
    expect(reconcileLiveReplies(held, phase('turn_running', 5))).toBe(held);
  });

  it('(a) retires a copy once its item/completed row is in the transcript, and only that', () => {
    const held = run(poll('T', { x: 'Hello', y: 'More' }));
    expect(texts(reconcileLiveReplies(held, transcript(1, [completed('x', 'item/started')]))))
      .toEqual(['T/x:Hello', 'T/y:More']);
    expect(texts(reconcileLiveReplies(held, transcript(1, [completed('x')])))).toEqual(['T/y:More']);
  });

  it('(b) retires a copy at a transcript whose newest page a read started after a non-streaming phase fetched', () => {
    const settled = run(poll('T', { x: 'Hello' }), phase('turn_completed', 4));
    expect(settled.map((copy) => copy.settledAt)).toEqual([4]);
    /* Read 4 had started when the phase was seen, however late its result lands. */
    expect(texts(reconcileLiveReplies(settled, transcript(4)))).toEqual(['T/x:Hello']);
    expect(reconcileLiveReplies(settled, transcript(5))).toEqual([]);
  });

  it('(b) treats every phase but the three streaming ones as the end, wedged and idle included', () => {
    for (const tag of ['idle', 'turn_completed', 'wedged', 'resumed', 'pending_thread_start'] as const) {
      expect(reconcileLiveReplies(run(poll('T', { x: 'Hello' }), phase(tag, 0)), transcript(1))).toEqual([]);
    }
    for (const tag of ['issuing_turn', 'turn_running', 'issuing_interrupt'] as const) {
      expect(texts(reconcileLiveReplies(run(poll('T', { x: 'Hello' }), phase(tag, 0)), transcript(1))))
        .toEqual(['T/x:Hello']);
    }
  });

  it('(b) keeps the first settling read, and does not settle copies that arrive after it', () => {
    const held = run(poll('T', { x: 'Hello' }), phase('turn_completed', 2), phase('idle', 7), poll('T', { y: 'late' }));
    expect(held.map((copy) => copy.settledAt)).toEqual([2, null]);
    expect(texts(reconcileLiveReplies(held, transcript(3)))).toEqual(['T/y:late']);
    expect(awaitsSettling(held, 'idle')).toBe(true);
    expect(awaitsSettling(held, 'turn_running')).toBe(false);
  });

  it('(c) retires the old turn\'s copies when a poll names another turn, but not on a null turn', () => {
    expect(texts(run(poll('T1', { x: 'old' }), poll(null, {}), poll('T2', { y: 'new' })))).toEqual(['T2/y:new']);
    expect(texts(run(poll('T1', { x: 'old' }), poll('T2', {})))).toEqual([]);
  });

  it('rejects a poll outside the confirmed active turn without changing its copies', () => {
    const held = run(poll('T2', { y: 'current' }));
    expect(reconcileLiveReplies(held, { ...poll('T1', { x: 'late' }), activeTurnId: 'T2' })).toBe(held);
    expect(reconcileLiveReplies(run(poll('T1', { x: 'old' })), { ...poll('T1', { x: 'late' }), activeTurnId: 'T2' })).toEqual([]);
    expect(texts(reconcileLiveReplies(held, { ...poll('T2', { y: 'current and growing' }), activeTurnId: 'T2' })))
      .toEqual(['T2/y:current and growing']);
  });

  it('retires old copies on a confirmed run identity and keeps an already matching copy stable', () => {
    const held = run(poll('T1', { x: 'old' }));
    expect(reconcileLiveReplies(held, { kind: 'active-turn', turnId: 'T1' })).toBe(held);
    expect(reconcileLiveReplies(held, { kind: 'active-turn', turnId: 'T2' })).toEqual([]);
  });

});

describe('liveReplyTurns', () => {
  it('draws each copy as an agent turn, trimmed as the stored reply is, skipping blank ones', () => {
    const held = run(poll('T', { x: '  Hello\n', y: ' \n' }, 42));
    expect(liveReplyTurns(held)).toEqual([{ id: 'live-T-x', author: 'agent', text: 'Hello', atMs: 42 }]);
  });
});

describe('replyMayStream', () => {
  it('names exactly the phases in which a reply may still be streaming', () => {
    const all: readonly HarnessPhaseTag[] = [
      'pending_thread_start', 'idle', 'issuing_turn', 'compacting', 'issuing_interrupt', 'turn_running', 'turn_completed', 'resumed', 'wedged',
    ];
    expect(all.filter(replyMayStream)).toEqual(['issuing_turn', 'issuing_interrupt', 'turn_running']);
    expect(replyMayStream(null)).toBe(false);
  });
});

describe('harnessLiveOperation', () => {
  it('reads the card\'s live replies and decodes the wire shape', () => {
    const operation = harnessLiveOperation('card/1');
    expect(operation).toMatchObject({ method: 'GET', path: '/api/cards/card%2F1/harness/live' });
    expect(operation.responseSchema.parse({ turn_id: null, items: [] })).toEqual({ turn_id: null, items: [] });
    expect(operation.responseSchema.safeParse({ turn_id: 'T', items: [{ item_id: 'x' }] }).success).toBe(false);
  });
});
