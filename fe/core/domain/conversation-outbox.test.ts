import { describe, expect, it } from 'vitest';

import type { ConversationTurn, TranscriptEntry } from './conversation.js';
import {
  beginSendOp, matchSendOps, outboxView, replacingTurn, settleSendOp, withConfirmedSends, withoutQueuedEntry,
  type SendOp, type SendOpPhase,
} from './conversation-outbox.js';

const row = (id: string, text = 'same'): ConversationTurn => ({ id, author: 'you', text, atMs: 1 });

const op = (key: string, phase: SendOpPhase, { before = 4, text = 'same', queued = false, entryId = null as string | null, atMs = 2 } = {}): SendOp => ({
  key, fromComposer: true, replaces: null,
  echo: { id: `echo-${key}`, author: 'you', text, atMs, serverHighWaterBefore: before, queued, entryId },
  ...phase,
});

const SENDING = { phase: 'sending', unknown: false } as const;
const CONFIRMED = { phase: 'confirmed' } as const;
const UNKNOWN_SPENT = { phase: 'failed', delivery: 'unknown', message: 'dropped' } as const;
const NOTHING_READ = { transcript: 0, run: 0 } as const;

const view = (ops: readonly SendOp[], server: readonly ConversationTurn[] = [], extra: Partial<Parameters<typeof outboxView>[0]> = {}) =>
  outboxView({
    serverEntries: server, serverTurns: server, liveReplies: [], queuedEntryIds: new Set(), stalled: false,
    ops, landed: NOTHING_READ, ...extra,
  });

const texts = (entries: readonly TranscriptEntry[]) => entries.map((entry) => 'text' in entry ? entry.text : entry.id);

describe('matching sends to persisted rows', () => {
  it('does not let an identical older row stand for a newer send', () => {
    expect(matchSendOps([row('4')], [op('a', CONFIRMED)]).size).toBe(0);
    expect([...matchSendOps([row('5')], [op('a', CONFIRMED)])]).toEqual(['a']);
  });

  it('lets one new row stand for only one of two identical sends, the older', () => {
    expect([...matchSendOps([row('5')], [op('b', CONFIRMED, { atMs: 3 }), op('a', CONFIRMED, { atMs: 2 })])]).toEqual(['a']);
  });
});

describe('the outbox view', () => {
  it('keeps a confirmed send until a read shows it, and retires it then', () => {
    expect(view([op('a', CONFIRMED)]).retire).toEqual([]);
    expect(texts(view([op('a', CONFIRMED)]).transcript)).toEqual(['same']);
    const shown = view([op('a', CONFIRMED)], [row('5')]);
    expect(shown.retire).toEqual(['a']);
    expect(texts(shown.transcript)).toEqual(['same']);
  });

  it('draws a confirmed send in the queue region while its claimed entry is listed, and does not retire it', () => {
    const listed = view([op('a', CONFIRMED, { entryId: 'e1', queued: true })], [], { queuedEntryIds: new Set(['e1']) });
    expect(listed.transcript).toEqual([]);
    expect(listed.retire).toEqual([]);
    expect(listed.blocked).toBe(false);
  });

  it('keeps a replayed send, claiming nothing, until a transcript and a run read started after its answer land', () => {
    const replayed = op('a', { phase: 'replayed', afterRead: 7 }, { entryId: null });
    expect(texts(view([replayed]).transcript)).toEqual(['same']);
    expect(view([replayed]).blocked).toBe(true);
    expect(view([replayed], [], { landed: { transcript: 8, run: 7 } }).retire).toEqual([]);
    expect(view([replayed], [], { landed: { transcript: 7, run: 9 } }).retire).toEqual([]);
    /* Whatever those reads show: here nothing, because the entry was disposed of meanwhile. */
    expect(view([replayed], [], { landed: { transcript: 8, run: 9 } }).retire).toEqual(['a']);
    /* A matching row hides it at once, but does not retire it before its reads. */
    const matched = view([replayed], [row('5')]);
    expect(matched.retire).toEqual([]);
    expect(texts(matched.transcript)).toEqual(['same']);
  });

  it('hides a spent unknown send that a read shows, keeping it with its Try again', () => {
    const shown = view([op('a', UNKNOWN_SPENT)], [row('5')]);
    expect(texts(shown.transcript)).toEqual(['same']);
    expect(shown.retire).toEqual([]);
    expect(shown.failed?.key).toBe('a');
    expect(shown.blocked).toBe(true);
    expect(texts(view([op('a', UNKNOWN_SPENT)]).transcript)).toEqual(['same']);
  });

  it('still blocks on a sending op a read already shows', () => {
    const shown = view([op('a', SENDING)], [row('5')]);
    expect(texts(shown.transcript)).toEqual(['same']);
    expect(shown.blocked).toBe(true);
    expect(shown.sending).toBe(true);
  });

  it('still draws a rejected send beside an equal row, which a stale read can reveal', () => {
    const rejected = op('a', { phase: 'failed', delivery: 'rejected', message: 'no' });
    expect(texts(view([rejected], [row('5')]).transcript)).toEqual(['same', 'same']);
  });

  it('licenses the queued caption only for a confirmed send of a queue that is not wedged', () => {
    const queued = op('a', CONFIRMED, { queued: true });
    const caption = (entries: readonly TranscriptEntry[]) => entries.map((entry) => 'queued' in entry && entry.queued);
    expect(caption(view([queued]).transcript)).toEqual([true]);
    expect(caption(view([queued], [], { stalled: true }).transcript)).toEqual([false]);
    expect(caption(view([op('a', { phase: 'sending', unknown: false }, { queued: true })]).transcript)).toEqual([false]);
  });

  it('remembers confirmed sends only', () => {
    const ops = [op('a', CONFIRMED, { atMs: 1 }), op('b', SENDING, { atMs: 2 })];
    expect(view(ops).confirmed.map((echo) => echo.id)).toEqual(['echo-a']);
    expect(view(ops).shown.map((echo) => echo.id)).toEqual(['echo-a', 'echo-b']);
    expect(view(ops).sending).toBe(true);
  });
});

describe('outbox transitions', () => {
  it('takes one send at a time, and a new press only past a refusal', () => {
    expect(beginSendOp([op('a', SENDING)], op('b', SENDING))).toBeNull();
    expect(beginSendOp([op('a', UNKNOWN_SPENT)], op('b', SENDING))).toBeNull();
    expect(beginSendOp([op('a', { phase: 'failed', delivery: 'refused', message: 'no' })], op('b', SENDING))?.map((held) => held.key))
      .toEqual(['b']);
    /* Try again resumes the failed op under its own key. */
    expect(beginSendOp([op('a', CONFIRMED), op('b', UNKNOWN_SPENT)], op('b', { phase: 'sending', unknown: true }))
      ?.map((held) => `${held.key}:${held.phase}`)).toEqual(['a:confirmed', 'b:sending']);
  });

  it('settles only an op still held', () => {
    const ops = [op('a', SENDING)];
    expect(settleSendOp(ops, 'gone', null)).toBe(ops);
    expect(settleSendOp(ops, 'a', null)).toEqual([]);
  });

  it('forgets a confirmed send whose queued entry was deleted', () => {
    const ops = [op('a', CONFIRMED, { entryId: 'e1' }), op('b', CONFIRMED, { entryId: 'e2' })];
    expect(withoutQueuedEntry(ops, 'e1').map((held) => held.key)).toEqual(['b']);
    expect(withoutQueuedEntry(ops, 'e3')).toBe(ops);
  });

  it('adds back to remembered entries the confirmed sends no row stands for yet', () => {
    const ops = [op('a', CONFIRMED, { before: 0 }), op('b', CONFIRMED, { before: 5 }), op('c', SENDING)];
    expect(texts(withConfirmedSends([row('5')], ops))).toEqual(['same', 'same']);
    expect(withConfirmedSends([row('5')], [])).toEqual([row('5')]);
  });
});

/* #2043: an Edit's Send is one op naming the turn it replaces; whether the server removes it only the server knows. */
describe('a send that replaces a turn', () => {
  const outcome = (id: string, turnId: string): TranscriptEntry =>
    ({ id, author: 'turn', turnId, status: 'completed', elapsedMs: null, atMs: 1 });
  const agent = (id: string, text: string): TranscriptEntry => ({ id, author: 'agent', text, atMs: 1 });
  /* The earlier turn, then the edited one: its prompt (row 3), reply and outcome. */
  const server: readonly TranscriptEntry[] = [
    row('1', 'Earlier'), agent('2', 'Earlier answer'), outcome('o-early', 'turn-0'),
    row('3', 'Original'), agent('4', 'Original answer'), outcome('o-edit', 'turn-1'),
  ];
  const replace = (phase: SendOpPhase): SendOp => ({
    ...op('r', phase, { text: 'Revised', before: 4 }), replaces: { turnId: 'turn-1', outcomeId: 'o-edit' },
  });
  const viewOf = (ops: readonly SendOp[], entries: readonly TranscriptEntry[] = server) => outboxView({
    serverEntries: entries, serverTurns: entries.filter((entry): entry is ConversationTurn => entry.author === 'you'),
    liveReplies: [], queuedEntryIds: new Set(), stalled: false, ops, landed: NOTHING_READ,
  });

  it('while out draws nothing and leaves the turn, which the caller marks', () => {
    const shown = viewOf([replace(SENDING)]);
    expect(texts(shown.transcript)).toEqual(texts(server));
    expect(shown.shown).toEqual([]);
    expect(shown.sending).toBe(true);
    expect(shown.blocked).toBe(true);
    expect(replacingTurn([replace(SENDING)])).toEqual({ turnId: 'turn-1', outcomeId: 'o-edit' });
    expect(replacingTurn([replace(CONFIRMED)])).toBeNull();
    expect(replacingTurn([op('plain', SENDING)])).toBeNull();
  });

  it.each([['confirmed', CONFIRMED], ['replayed', { phase: 'replayed', afterRead: 1 } as const]] as const)(
    'once answered (%s) hides the turn a stale read still shows, and draws its message', (_, phase) => {
      const shown = viewOf([replace(phase)]);
      expect(texts(shown.transcript)).toEqual(['Earlier', 'Earlier answer', 'o-early', 'Revised']);
      /* A read without the turn is drawn as it is. */
      expect(texts(viewOf([replace(phase)], server.slice(0, 3)).transcript)).toEqual(['Earlier', 'Earlier answer', 'o-early', 'Revised']);
    });

  it('leaves the turn as the server has it when the replace failed', () => {
    for (const delivery of ['unknown', 'refused', 'rejected'] as const) {
      const shown = viewOf([replace({ phase: 'failed', delivery, message: 'no' })]);
      expect(texts(shown.transcript).slice(0, 6)).toEqual(texts(server));
    }
  });

  it('hides the replaced turn from the remembered entries while its confirmed message waits for a read', () => {
    expect(texts(withConfirmedSends(server, [replace(CONFIRMED)]))).toEqual(['Earlier', 'Earlier answer', 'o-early', 'Revised']);
    expect(withConfirmedSends(server, [replace(SENDING)])).toBe(server);
  });
});
