import { describe, expect, it } from 'vitest';

import type { ConversationTurn, TranscriptEntry } from './conversation.js';
import {
  beginSendOp, markedReplace, matchSendOps, outboxView, replacingTurn, settleSendOp, withConfirmedSends, withPendingMessages, synchronizeQueuedSends, presentPendingMessages, rememberQueuePlacements, withoutQueuedEntry,
  type SendOp, type SendOpPhase,
} from './conversation-outbox.js';

const row = (id: string, text = 'same'): ConversationTurn => ({ id, author: 'you', text, atMs: 1 });

const op = (key: string, phase: SendOpPhase, { before = 4, text = 'same', queued = false, entryId = null as string | null, atMs = 2 } = {}): SendOp => ({
  key, fromComposer: true, replaces: null,
  echo: { id: `echo-${key}`, author: 'you', text, atMs, serverHighWaterBefore: before, queued, entryId },
  ...phase,
});

const SENDING = { phase: 'sending', unknown: false } as const;
const CONFIRMED = { phase: 'confirmed', queueRevision: null } as const;
const UNKNOWN_SPENT = { phase: 'failed', delivery: 'unknown', message: 'dropped' } as const;
const NOTHING_READ = { transcript: 0, run: 0 } as const;
const NONE_SPENT: readonly string[] = [];

const view = (ops: readonly SendOp[], server: readonly ConversationTurn[] = [], extra: Partial<Parameters<typeof outboxView>[0]> = {}) =>
  outboxView({
    serverEntries: server, serverTurns: server, liveReplies: [], queuedEntryIds: new Set(), stalled: false,
    ops, spent: NONE_SPENT, landed: NOTHING_READ, ...extra,
  });

const texts = (entries: readonly TranscriptEntry[]) => entries.map((entry) => 'text' in entry ? entry.text : entry.id);

describe('matching sends to persisted rows', () => {
  it('does not let an identical older row stand for a newer send', () => {
    expect(matchSendOps([row('4')], [op('a', CONFIRMED)], NONE_SPENT).size).toBe(0);
    expect([...matchSendOps([row('5')], [op('a', CONFIRMED)], NONE_SPENT)]).toEqual([['a', '5']]);
  });

  it('lets one new row stand for only one of two identical sends, the older', () => {
    expect([...matchSendOps([row('5')], [op('b', CONFIRMED, { atMs: 3 }), op('a', CONFIRMED, { atMs: 2 })], NONE_SPENT)])
      .toEqual([['a', '5']]);
  });

  /* #2068 item 8: over its lifetime, not only within one read. */
  it('never lets a row that already retired a send stand for another', () => {
    expect(matchSendOps([row('5')], [op('b', UNKNOWN_SPENT)], ['5']).size).toBe(0);
  });
});

describe('the outbox view', () => {
  it('declares only the existing answered-send reconciliation gate as awaiting reads', () => {
    expect(view([op('a', CONFIRMED)]).awaitingReconciliation).toBe(true);
    expect(view([op('a', { phase: 'replayed', afterRead: 7 })]).awaitingReconciliation).toBe(true);
    expect(view([op('a', CONFIRMED, { queued: true })]).awaitingReconciliation).toBe(false);
    expect(view([op('a', SENDING)]).awaitingReconciliation).toBe(false);
    expect(view([op('a', UNKNOWN_SPENT)]).awaitingReconciliation).toBe(false);
    expect(view([op('a', CONFIRMED)], [row('5')]).awaitingReconciliation).toBe(false);
    expect(view([op('a', CONFIRMED)], [], { stalled: true }).awaitingReconciliation).toBe(false);
  });
  it('keeps a confirmed send until a read shows it, and retires it then', () => {
    expect(view([op('a', CONFIRMED)]).retire).toEqual([]);
    expect(texts(view([op('a', CONFIRMED)]).transcript)).toEqual(['same']);
    const shown = view([op('a', CONFIRMED)], [row('5')]);
    expect(shown.retire).toEqual([{ key: 'a', row: '5' }]);
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
    expect(view([replayed], [], { landed: { transcript: 8, run: 9 } }).retire).toEqual([{ key: 'a', row: null }]);
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
  it('takes one send at a time, and no new press while a failed one waits', () => {
    expect(beginSendOp([op('a', SENDING)], op('b', SENDING))).toBeNull();
    expect(beginSendOp([op('a', UNKNOWN_SPENT)], op('b', SENDING))).toBeNull();
    expect(beginSendOp([op('a', { phase: 'failed', delivery: 'rejected', message: 'no' })], op('b', SENDING))).toBeNull();
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
    expect(texts(withConfirmedSends([row('5')], ops, NONE_SPENT))).toEqual(['same', 'same']);
    expect(texts(withConfirmedSends([row('5')], ops, ['5']))).toEqual(['same', 'same', 'same']);
    expect(withConfirmedSends([row('5')], [], NONE_SPENT)).toEqual([row('5')]);
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
    liveReplies: [], queuedEntryIds: new Set(), stalled: false, ops, spent: NONE_SPENT, landed: NOTHING_READ,
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

  it('marks the turn a replace names while it is out or failed, never once answered', () => {
    for (const phase of [SENDING, UNKNOWN_SPENT]) expect(markedReplace([replace(phase)])).toEqual({ turnId: 'turn-1', outcomeId: 'o-edit' });
    expect(markedReplace([replace(CONFIRMED)])).toBeNull();
    expect(markedReplace([op('plain', UNKNOWN_SPENT)])).toBeNull();
  });

  it.each([['confirmed', CONFIRMED], ['replayed', { phase: 'replayed', afterRead: 1 } as const]] as const)(
    'once answered (%s) hides the turn a stale read still shows, and draws its message', (_, phase) => {
      const shown = viewOf([replace(phase)]);
      expect(texts(shown.transcript)).toEqual(['Earlier', 'Earlier answer', 'o-early', 'Revised']);
      /* A read without the turn is drawn as it is. */
      expect(texts(viewOf([replace(phase)], server.slice(0, 3)).transcript)).toEqual(['Earlier', 'Earlier answer', 'o-early', 'Revised']);
    });

  it('leaves the turn as the server has it when the replace failed', () => {
    for (const delivery of ['unknown', 'rejected'] as const) {
      const shown = viewOf([replace({ phase: 'failed', delivery, message: 'no' })]);
      expect(texts(shown.transcript).slice(0, 6)).toEqual(texts(server));
    }
  });

  it('hides the replaced turn from the remembered entries while its confirmed message waits for a read', () => {
    expect(texts(withConfirmedSends(server, [replace(CONFIRMED)], NONE_SPENT))).toEqual(['Earlier', 'Earlier answer', 'o-early', 'Revised']);
    expect(withConfirmedSends(server, [replace(SENDING)], NONE_SPENT)).toBe(server);
  });
});


describe('pending inputs in the transcript', () => {
  const waiting = { entry_id: 'queue-a', text: 'same', rev: 1, queued_at_ms: 100 };
  it('retains the confirmed echo identity and its attachments through queue acknowledgement', () => {
    const echo = { ...op('a', CONFIRMED, { entryId: waiting.entry_id }).echo,
      attachments: [{ id: 'image', url: '/image.png', contentType: 'image/png', size: 1 }] };
    const projected = withPendingMessages([], [waiting], [echo]);
    expect(projected).toEqual([{ ...echo, queued: true }]);
    expect(projected[0].id).toBe(echo.id);
  });
  it('keeps two identical messages distinct when only one has a confirmed entry id', () => {
    const unconfirmed = op('unknown', { phase: 'sending', unknown: true }).echo;
    const projected = withPendingMessages([unconfirmed], [waiting], [unconfirmed]);
    expect(projected).toHaveLength(2);
    expect(projected[0]).toBe(unconfirmed);
    expect(projected[1]).toMatchObject({ id: 'queued-queue-a', text: 'same', queued: true, entryId: 'queue-a' });
  });
  it('updates an existing queued bubble once, preserving its place and identity', () => {
    const echo = op('a', CONFIRMED, { entryId: waiting.entry_id }).echo;
    const entries = [row('1', 'earlier'), echo];
    const projected = withPendingMessages(entries, [{ ...waiting, text: 'updated queue text' }], [echo]);
    expect(projected).toHaveLength(2);
    expect(projected[0]).toBe(entries[0]);
    expect(projected[1]).toMatchObject({ id: echo.id, text: 'updated queue text', queued: true });
    expect(echo.text).toBe('same');
    expect(withPendingMessages(entries, [], [echo])).toBe(entries);
  });
});


describe('waiting input presentation', () => {
  const queue = { entry_id: 'entry', text: 'later question', rev: 4, queued_at_ms: 100 };
  const streaming: ConversationTurn = { id: 'live', author: 'agent', text: 'still streaming', atMs: 1 };
  it('keeps an outgoing waiting message out of a streaming transcript before acknowledgement', () => {
    const echo = op('later', SENDING, { text: queue.text, queued: true }).echo;
    const drawn = { ...echo, queued: false };
    const shown = presentPendingMessages([streaming, drawn], [], [echo], new Map(), null, false, [op('later', SENDING, { text: queue.text, queued: true })]);
    expect(shown.transcript).toEqual([streaming]);
    expect(shown.sending).toEqual([{ ...echo, delivery: 'sending' }]);
  });
  it('keeps confirmed waiting inputs separate and does not interleave them with a live reply', () => {
    const echo = op('later', CONFIRMED, { text: queue.text, queued: true, entryId: queue.entry_id }).echo;
    const shown = presentPendingMessages([streaming], [queue], [echo], new Map(), null, false, []);
    expect(shown.transcript).toEqual([streaming]); expect(shown.queue).toEqual([queue]); expect(shown.sending).toEqual([]);
  });
  it('keeps an idle send inline until its queue snapshot disappears, even after its outbox retires', () => {
    const sent = op('idle', CONFIRMED, { text: queue.text, queued: false, entryId: queue.entry_id });
    let placements = rememberQueuePlacements(new Map(), [sent], [queue]);
    placements = rememberQueuePlacements(placements, [], [queue]);
    const shown = presentPendingMessages([], [queue], [], placements, null, false, []);
    expect(shown.queue).toEqual([]); expect(shown.transcript).toMatchObject([{ text: queue.text, queued: false }]);
  });
  it('restores waiting controls when a previously inline input is rebuffered at a newer revision', () => {
    const sent = op('idle', CONFIRMED, { queued: false, entryId: queue.entry_id });
    let placements = rememberQueuePlacements(new Map(), [sent], [queue]);
    placements = rememberQueuePlacements(placements, [], [{ ...queue, rev: 6 }]);
    const shown = presentPendingMessages([streaming], [{ ...queue, rev: 6 }], [], placements, null, false, []);
    expect(shown.transcript).toEqual([streaming]); expect(shown.queue).toMatchObject([{ entry_id: queue.entry_id, rev: 6 }]);
  });
  it('keeps a failed waiting send outside the streaming transcript with explicit failure feedback', () => {
    const failed = op('failed', { phase: 'failed', delivery: 'unknown', message: 'lost' }, { queued: true });
    if (failed.phase !== 'failed') throw new Error('fixture must be failed');
    const drawn = { ...failed.echo, queued: false };
    const shown = presentPendingMessages([streaming, drawn], [], [], new Map(), failed, false, [failed]);
    expect(shown.transcript).toEqual([streaming]); expect(shown.sending).toEqual([{ ...failed.echo, delivery: 'failed' }]);
  });
});


it('does not promise dispatch for an accepted input while the runtime is paused', () => {
  const echo = op('paused', CONFIRMED, { queued: true, entryId: 'entry' }).echo;
  const shown = presentPendingMessages([], [], [echo], new Map(), null, true, []);
  expect(shown.transcript).toEqual([]);
  expect(shown.sending).toEqual([{ ...echo, delivery: 'paused' }]);
});


it('uses acknowledged operation state even when an accepted legacy receipt has no entry id', () => {
  const accepted = op('accepted', CONFIRMED, { queued: true, entryId: null });
  const shown = presentPendingMessages([], [], [accepted.echo], new Map(), null, false, [accepted]);
  expect(shown.sending).toEqual([{ ...accepted.echo, delivery: 'waiting' }]);
});


it('retires an edited queued send against the revised transcript text without leaving a phantom input', () => {
  const sent = op('edited', CONFIRMED, { text: 'old middle text', queued: true, entryId: 'middle' });
  const edited = synchronizeQueuedSends([sent], [{ entry_id: 'middle', text: 'new middle text', rev: 1, queued_at_ms: 10 }]);
  const shown = view(edited, [row('5', 'new middle text')]);
  expect(shown.retire).toEqual([{ key: sent.key, row: '5' }]);
  expect(shown.transcript).toEqual([row('5', 'new middle text')]);
  expect(shown.shown).toEqual([]);
});

it('does not roll a successful queue edit back to an older cached queue revision', () => {
  const sent = op('edited', CONFIRMED, { text: 'before', queued: true, entryId: 'middle' });
  const fresh = synchronizeQueuedSends([sent], [{ entry_id: 'middle', text: 'after', rev: 2, queued_at_ms: 10 }]);
  const stale = synchronizeQueuedSends(fresh, [{ entry_id: 'middle', text: 'before', rev: 1, queued_at_ms: 10 }]);
  expect(stale).toBe(fresh);
  expect(stale[0].echo.text).toBe('after');
});

it('updates only the confirmed queue identity when equal messages coexist', () => {
  const confirmed = op('known', CONFIRMED, { queued: true, entryId: 'known' });
  const unconfirmed = op('unknown', SENDING, { queued: true });
  const shown = synchronizeQueuedSends([confirmed, unconfirmed], [{ entry_id: 'known', text: 'edited', rev: 1, queued_at_ms: 10 }]);
  expect(shown[0].echo.text).toBe('edited'); expect(shown[1]).toBe(unconfirmed);
});

it('keeps accepted first-input evidence outside ordinary queue placement and send delivery helpers', () => {
  const initial: import('./conversation-outbox.js').AcceptedInitialInput = {
    phase: 'initial-accepted', localWrite: null, sourceWitness: null, key: 'create', trackId: 'track', submitted: { text: 'first input', atMs: 1 },
    receipt: { version: 'initial-input-v1', entryId: 'first-entry', queuedAtMs: 2,
      textSha256: 'a'.repeat(64), initialRuntimeId: 'runtime' },
  };
  const first: ConversationTurn = { id: 'initial-create', author: 'you', text: initial.submitted.text, atMs: 1 };
  const later = op('later', SENDING, { text: 'later input', queued: true });
  const operations = [initial, later];
  const drawn = { ...later.echo, queued: false };
  const shown = presentPendingMessages([first, drawn], [], [first, later.echo],
    rememberQueuePlacements(new Map(), operations, []), null, false, operations);
  expect(shown.transcript).toEqual([first]);
  expect(shown.queue).toEqual([]);
  expect(shown.sending).toEqual([{ ...later.echo, delivery: 'sending' }]);
  expect(synchronizeQueuedSends(operations, [{ entry_id: 'first-entry', text: 'edited first input', rev: 1, queued_at_ms: 2 }])[0]).toBe(initial);
  expect(rememberQueuePlacements(new Map(), operations, []).has('first-entry')).toBe(false);
});
