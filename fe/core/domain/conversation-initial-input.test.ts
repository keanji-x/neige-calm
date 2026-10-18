import { mergeTranscript, type ConversationTurn, type TranscriptEntry } from './conversation.js';
import { describe, expect, it } from 'vitest';
import type { ConversationInitialInputObservation } from '../api/generated/wire.js';
import { initialInputObservationOperation, observationMatchesReceipt, receiptMatchesSubmission } from './conversation-initial-input.js';
import { outboxView, initialInputQueueView, initialInputEcho, matchSendOps, acknowledgeQueueMutation, withInitialInputSources, initialInputNeedsObservation, withInitialInputFacts, currentInitialInput, withConfirmedSends, initialInputDisplaced, queueDisplayAccounting, queueEntryIsAccounted, type AcceptedInitialInput, type ObservedInitialInput } from './conversation-outbox.js';
import { sha256Hex } from './sha256.js';

const RAW_SCOPE = Object.freeze({ cardId: 'card', threadId: 'thread' });

const receipt = Object.freeze({ version: 'initial-input-v1' as const, entryId: 'first-entry', entryRev: 0, queuedAtMs: 100,
  textSha256: sha256Hex('first words'), initialRuntimeId: 'runtime' });
const accepted: AcceptedInitialInput = Object.freeze({ phase: 'initial-accepted', key: 'original-key', trackId: 'track',
  submitted: Object.freeze({ text: 'first words', atMs: 2 }), receipt, localWrite: null, inputFact: null, sourceWitness: null });
const observed = (observation: ConversationInitialInputObservation, afterRead = 10): ObservedInitialInput => ({ observation, afterRead });
const queued = (text = 'first words', rev = 0): ConversationInitialInputObservation => ({ kind: 'queued', receipt,
  runtimeId: 'runtime', entryId: receipt.entryId, rev, text, textSha256: sha256Hex(text) });
const projected: ConversationInitialInputObservation = { kind: 'projected', entryRev: 0, receipt, runtimeId: 'runtime', itemId: 70, segmentIndex: 2 };
const view = (observation?: ObservedInitialInput, transcriptRead = 0,
  extra: Partial<Parameters<typeof outboxView>[0]> = {}) => outboxView({ cardId: 'card',
  serverEntries: [], serverTurns: [], liveReplies: [], queuedEntryIds: new Set(), stalled: false, ops: [accepted], spent: [],
  landed: { transcript: transcriptRead, run: 1000 }, initialInput: observation === undefined ? new Map() : new Map([[accepted.key, observation]]), ...extra,
});

describe('accepted first-message continuity', () => {
  it('keeps provisional words through arbitrarily newer empty history/run reads without claiming sending or queued', () => {
    const shown = view(undefined, 1000);
    expect(shown.retire).toEqual([]);
    expect(shown.transcript).toEqual([{ id: 'initial-original-key', author: 'you', text: 'first words', atMs: 2, queueEntryId: receipt.entryId }]);
    expect(shown.sending).toBe(false);
    expect(shown.failed).toBeNull();
    expect(shown.confirmed).toEqual([]);
    expect(shown.checkingInitialInput).toBe(true);
    expect(shown.blocked).toBe(false);
  });

  it('uses the real pending region for a coherently queued entry and permits ordinary input', () => {
    const shown = view(observed(queued()), 1000, { queuedEntryIds: new Set([receipt.entryId]) });
    expect(shown.transcript).toEqual([]);
    expect(shown.retire).toEqual([]);
    expect(shown.blocked).toBe(false);
    expect(shown.checkingInitialInput).toBe(false);
  });

  it('hides changed original words immediately and retains nondrawing bookkeeping until both fresh sources land', () => {
    for (const observation of [queued('edited words', 1)]) {
      const waiting = view(observed(observation));
      expect(waiting.retire).toEqual([]);
      expect(waiting.transcript).toEqual([]);
      expect(waiting.blocked).toBe(false);
      expect(view(observed(observation), 11).retire).toEqual([{ key: accepted.key, row: null }]);
    }
    expect(view(observed({ kind: 'unavailable', reason: 'shutting-down', receipt }), 1000).retire).toEqual([]);
  });

  it('uses coherent queue evidence only against older cached pages and permits a newer restored entry', () => {
    const entry = { entry_id: receipt.entryId, rev: 0, text: 'first words', queued_at_ms: receipt.queuedAtMs };
    for (const observation of [queued('edited words', 1), projected]) {
      if (observation.kind === 'unavailable') throw new Error('Expected an actual queued or projected observation');
      const prior = initialInputQueueView([entry], accepted, observed(observation), 9);
      expect(prior.entries).toEqual([]);
      expect(prior.suppressedRefs).toEqual([{ id: receipt.entryId, rev: observation.kind === 'queued' ? observation.rev : observation.entryRev }]);
      expect(initialInputQueueView([entry], accepted, observed(observation), 11).entries).toEqual([entry]);
    }
    expect(initialInputQueueView([entry], accepted, observed(queued()), 9).entries).toEqual([entry]);
  });

  it('waits for successful history started after projection observation, including a carrier outside the loaded page', () => {
    expect(view(observed(projected), 10).retire).toEqual([]);
    expect(view(observed(projected), 9).transcript).toHaveLength(1);
    expect(view(observed(projected), 11).retire).toEqual([{ key: accepted.key, row: null }]);
    expect(view(observed(projected), 11).transcript).toEqual([]);
  });

  it('deduplicates a raw exact carrier but retains its claim until coherent projected evidence', () => {
    const carrier = { id: '70:2', author: 'you' as const, text: 'combined words', atMs: 3, queueEntryId: receipt.entryId, inputSource: { ...RAW_SCOPE, itemId: 70, segmentIndex: 2, turnId: 'accepted-turn', entryRev: 0 } };
    const extra = { serverEntries: [carrier], serverTurns: [carrier], spent: ['70:2'] };
    expect(view(undefined, 0, extra).retire).toEqual([]);
    expect(view(undefined, 0, extra).transcript).toEqual([carrier]);
    const older = { ...carrier, inputSource: { ...RAW_SCOPE, itemId: 69, segmentIndex: 2, turnId: 'accepted-turn', entryRev: 0 } };
    expect(view(observed(projected), 0, { serverEntries: [older], serverTurns: [older] }).retire).toEqual([]);
    expect(view(observed(projected), 11, { serverEntries: [older], serverTurns: [older] }).retire).toEqual([]);
    const shown = view(observed(projected), 0, extra);
    expect(shown.retire).toEqual([]);
    expect(shown.transcript).toEqual([carrier]);
    const rebuffered = view(observed(queued()), 9, extra);
    expect(rebuffered.retire).toEqual([]);
    expect(rebuffered.transcript.map(entry => 'text' in entry ? entry.text : '')).toEqual(['first words']);
    const newerHistory = view(observed(queued()), 11, extra);
    expect(newerHistory.retire).toEqual([]);
    expect(newerHistory.transcript).toEqual([carrier]);
    expect(view(observed(queued()), 11, { ...extra, serverEntries: [], queuedEntryIds: new Set([receipt.entryId]) }).transcript).toEqual([]);
  });

  it('rejects missing, unsupported and inconsistent observation receipts rather than treating them as absence', () => {
    const operation = initialInputObservationOperation('track with space', 'card', 'original-key');
    expect(operation.headers).toEqual({ 'Idempotency-Key': 'original-key' });
    expect(operation.path).toBe('/api/tracks/track%20with%20space/conversations/card/initial-input');
    for (const invalid of [
      { kind: 'projected', runtimeId: 'runtime', itemId: 70, segmentIndex: 2 },
      { kind: 'queued', receipt: { ...receipt, version: 'future' }, runtimeId: 'runtime', entryId: receipt.entryId, rev: 0, text: 'first words', textSha256: receipt.textSha256 },
      { kind: 'unavailable', reason: 'runtime-unavailable' },
      { kind: 'unavailable', reason: 'runtime-unavailable', receipt: null },
      { kind: 'unavailable', reason: 'creation-incomplete', receipt },
      { ...queued(), entryId: 'different-entry' },
      { ...queued(), textSha256: sha256Hex('different words') },
    ]) expect(operation.responseSchema.safeParse(invalid).success).toBe(false);
    expect(receiptMatchesSubmission(receipt, { text: 'edited after send', atMs: 2 })).toBe(false);
  });
});


it('retains one terminal acknowledged displacement across fresh page misses without changing raw memory', () => {
  const removed = { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: null };
  const ops = acknowledgeQueueMutation([accepted], removed);
  const carrier = { id: '70:2', author: 'you' as const, text: 'first words', atMs: 2, queueEntryId: receipt.entryId, inputSource: { ...RAW_SCOPE, itemId: 70, segmentIndex: 2, turnId: null, entryRev: 0 } };
  expect(withConfirmedSends([carrier], ops, [], 'card')).toEqual([]);
  for (const landed of [{ transcript: 19, run: 19 }, { transcript: 21, run: 19 }, { transcript: 19, run: 21 }]) {
    const shown = view(observed(queued()), 0, { ops, serverEntries: [carrier], serverTurns: [carrier], landed });
    expect(shown.transcript).toEqual([]); expect(shown.retire).toEqual([]);
  }
  expect(view(observed(queued()), 21, { ops }).retire).toEqual([]);
  expect(accepted.localWrite).toBeNull();
});

it('binds local writes by entry identity and stored revision, preserving same-text edits and newer restored queue entries', () => {
  const other = acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: 'another-entry', rev: 1, text: null });
  expect(other[0]).toBe(accepted);
  const same = acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: accepted.submitted.text });
  const edited = same[0];
  if (edited.phase !== 'initial-accepted') throw new Error('Expected original acceptance');
  expect(initialInputDisplaced(edited, observed(queued()))).toBe(false);
  expect(withConfirmedSends([], same, [], 'card')).toMatchObject([{ text: accepted.submitted.text }]);
  const removed = acknowledgeQueueMutation(same, { card_id: 'card', entry_id: receipt.entryId, rev: 2, text: null });
  const lateEdit = acknowledgeQueueMutation(removed, { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: accepted.submitted.text });
  expect(lateEdit[0]).toBe(removed[0]);
  const op = removed[0];
  if (op.phase !== 'initial-accepted') throw new Error('Expected retained displacement');
  const entry = { entry_id: receipt.entryId, text: 'A restored actual queue entry', queued_at_ms: 100, rev: 3 };
  expect(initialInputQueueView([{ ...entry, rev: 2 }], op, observed(queued()), 21).entries).toEqual([]);
  expect(initialInputQueueView([entry], op, observed(queued()), 21).entries).toEqual([entry]);
});

it('rejects an unproven absence claim while original acceptance remains unresolved', () => {
  const operation = initialInputObservationOperation('track', 'card', 'original-key');
  expect(operation.responseSchema.safeParse({ kind: 'absent', receipt, runtimeId: 'runtime' }).success).toBe(false);
});


it('lets an actual newer acknowledged revision outrank a delayed older queued observation', () => {
  const result = { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: 'Locally revised first input' };
  const ops = acknowledgeQueueMutation([accepted], result);
  const op = ops[0];
  if (op.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const actual = { entry_id: receipt.entryId, rev: 1, text: result.text, queued_at_ms: 100 };
  expect(initialInputQueueView([actual], op, observed(queued(), 30), 21).entries).toEqual([actual]);
});


it('preserves real bound history and the exact ACK-edited unbound body while hiding only obsolete unbound input', () => {
  const old = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 2,
    queueEntryId: receipt.entryId, inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: null, entryRev: 0 } };
  const bound = { ...old, id: '8:0', inputSource: { ...RAW_SCOPE, itemId: 8, segmentIndex: 0, turnId: 'real-provider-turn', entryRev: 0 } };
  const revised = { ...old, id: '9:0', text: 'Actual acknowledged revised words', inputSource: { ...RAW_SCOPE, itemId: 9, segmentIndex: 0, turnId: null, entryRev: 1 } };
  const result = { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: revised.text };
  const ops = acknowledgeQueueMutation([accepted], result);
  const entries = [old, bound, revised];
  const shown = view(observed(queued()), 1000, { ops, serverEntries: entries, serverTurns: entries });
  expect(shown.transcript).toEqual([bound, revised]);
  expect(shown.retire).toEqual([]);
  expect(withConfirmedSends(entries, ops, [], 'card')).toEqual([bound, revised]);
});


it('never lets the receipt-identified accepted display certify its own raw canonical carrier', () => {
  const display = initialInputEcho(accepted);
  expect(display.queueEntryId).toBe(receipt.entryId);
  expect(display.inputSource).toBeUndefined();
  expect(matchSendOps([display], [accepted], [], 'card').size).toBe(0);
});


it('lets a same-body PATCH receipt outrank an older changed queued inspection without retiring original acceptance', () => {
  const ops = acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: receipt.entryId, rev: 2, text: accepted.submitted.text });
  const op = ops[0];
  if (op.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const old = observed(queued('Older changed body', 1), 30);
  expect(initialInputDisplaced(op, old)).toBe(false);
  expect(view(old, 1000, { ops }).retire).toEqual([]);
});


it('transfers display ownership only from an admitted exact provider-bound raw source and retains one positive witness across page misses and later ACKs', () => {
  const source = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 2,
    queueEntryId: receipt.entryId, inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: 'accepted-turn', entryRev: 0 } };
  expect(withInitialInputSources([accepted], [initialInputEcho(accepted)] )[0]).toBe(accepted);
  expect(withInitialInputSources([accepted], [{ ...source, queueEntryId: 'foreign' }])[0]).toBe(accepted);
  const fenced = view(observed(queued()), 9, { serverEntries: [source], serverTurns: [source] });
  expect(withInitialInputSources([accepted], fenced.transcript)[0]).toBe(accepted);
  const shown = view(observed(queued()), 11, { serverEntries: [source], serverTurns: [source] });
  const handed = withInitialInputSources([accepted], shown.transcript);
  const op = handed[0]; if (op.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(op.sourceWitness).toEqual(source.inputSource); expect(initialInputNeedsObservation(op)).toBe(false);
  expect(withInitialInputSources(handed, [])).toBe(handed);
  expect(view(observed(queued(), 100), 11, { ops: handed, serverEntries: [source], serverTurns: [source] }).transcript).toEqual([source]);
  expect(view(undefined, 1000, { ops: handed }).transcript).toEqual([]);
  expect(withConfirmedSends([], handed, [], 'card')).toEqual([]);
  const removed = acknowledgeQueueMutation(handed, { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: null });
  expect(removed[0]).toMatchObject({ sourceWitness: source.inputSource, localWrite: { text: null } });
  expect(view(undefined, 1000, { ops: removed, serverEntries: [source], serverTurns: [source] }).transcript).toEqual([source]);
  expect(initialInputNeedsObservation(accepted)).toBe(true);
});


it('uses actual PATCH body and revision only for an existing cached queue row, letting an actual newer revision or queue absence win', () => {
  const old = { entry_id: receipt.entryId, text: accepted.submitted.text, rev: 0, queued_at_ms: receipt.queuedAtMs };
  const saved = { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: 'Actual stored revised body' };
  const op = acknowledgeQueueMutation([accepted], saved)[0];
  if (op.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const queuedView = initialInputQueueView([old], op, observed(queued()), 0);
  expect(queuedView.entries).toEqual([{ ...old, text: saved.text, rev: saved.rev }]);
  expect(queuedView.suppressedRefs).toEqual([]);
  expect(initialInputQueueView([], op, observed(queued()), 1000).entries).toEqual([]);
  const newer = { ...old, text: 'Actual later revision', rev: 2 };
  expect(initialInputQueueView([newer], op, observed(queued()), 1000).entries).toEqual([newer]);
  expect(initialInputEcho(op)).toMatchObject({ text: saved.text, queueEntryId: receipt.entryId });
  expect(initialInputEcho(op).inputSource).toBeUndefined();
  expect(withConfirmedSends([], [op], [], 'card')).toEqual([initialInputEcho(op)]);
});

it('hands off only the current actual ACK body and ends creation observation through same-body and restored-original edits', () => {
  const originalSource = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 2,
    queueEntryId: receipt.entryId, inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: 'old-provider-turn', entryRev: 0 } };
  const witnessed = withInitialInputSources([accepted], [originalSource]);
  const same = acknowledgeQueueMutation(witnessed, { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: accepted.submitted.text });
  expect(same[0]).toMatchObject({ sourceWitness: originalSource.inputSource });
  const plainSame = acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: accepted.submitted.text })[0];
  if (plainSame.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(initialInputNeedsObservation(plainSame)).toBe(false);
  const edited = acknowledgeQueueMutation(same, { card_id: 'card', entry_id: receipt.entryId, rev: 2, text: 'Actual revised body' });
  expect(edited[0]).toMatchObject({ sourceWitness: originalSource.inputSource });
  expect(withInitialInputSources(edited, [originalSource])).toBe(edited);
  expect(withConfirmedSends([originalSource], edited, [], 'card').map(entry => 'text' in entry ? entry.text : entry.author)).toEqual([accepted.submitted.text, 'Actual revised body']);
  const editedSource = { ...originalSource, id: '8:0', text: 'Actual revised body', inputSource: { ...RAW_SCOPE, itemId: 8, segmentIndex: 0, turnId: 'revised-provider-turn', entryRev: 2 } };
  const handed = withInitialInputSources(edited, [originalSource, editedSource]);
  expect(handed[0]).toMatchObject({ sourceWitness: editedSource.inputSource });
  expect(withConfirmedSends([], handed, [], 'card')).toEqual([]);
  const restored = acknowledgeQueueMutation(handed, { card_id: 'card', entry_id: receipt.entryId, rev: 3, text: accepted.submitted.text })[0];
  if (restored.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(restored.sourceWitness).toEqual(editedSource.inputSource); expect(initialInputNeedsObservation(restored)).toBe(false);
  expect(initialInputEcho(restored).text).toBe(accepted.submitted.text);
});


it('selects one latest actual queue/write authority and never reclaims an off-page source for an older or repeated fact', () => {
  const source = { id: '7:0', author: 'you' as const, text: 'Actual known current body', atMs: 2,
    queueEntryId: receipt.entryId, inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: 'current-provider-turn', entryRev: 2 } };
  const fact = { kind: 'queue' as const, entryId: receipt.entryId, rev: 2, text: source.text };
  const known = withInitialInputFacts(accepted, [fact]);
  expect(currentInitialInput(known)).toEqual({ kind: 'observed', rev: 2, text: source.text });
  const handed = withInitialInputSources([known], [source])[0];
  if (handed.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(withInitialInputFacts(handed, [fact])).toBe(handed);
  const olderWrite = acknowledgeQueueMutation([handed], { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: 'Older ACK' })[0];
  if (olderWrite.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(olderWrite.sourceWitness).toEqual(source.inputSource);
  expect(currentInitialInput(olderWrite)).toEqual({ kind: 'observed', rev: 2, text: source.text });
  expect(withConfirmedSends([], [olderWrite], [], 'card')).toEqual([]);
  const newer = withInitialInputFacts(olderWrite, [{ ...fact, rev: 3 }]);
  expect(newer.sourceWitness).toEqual(source.inputSource);
  expect(initialInputEcho(newer).text).toBe(source.text);
  const removed = acknowledgeQueueMutation([newer], { card_id: 'card', entry_id: receipt.entryId, rev: 3, text: null })[0];
  if (removed.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(currentInitialInput(removed)).toEqual({ kind: 'write', rev: 3, text: null });
  const restored = withInitialInputFacts(removed, [{ ...fact, rev: 4 }]);
  expect(currentInitialInput(restored)).toEqual({ kind: 'observed', rev: 4, text: source.text });
  expect(initialInputEcho(restored).text).toBe(source.text);
  expect(initialInputNeedsObservation(restored)).toBe(false);
  const newerAck = acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: receipt.entryId, rev: 5, text: source.text })[0];
  if (newerAck.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const ackSource = withInitialInputSources([newerAck], [{ ...source, inputSource: { ...source.inputSource, entryRev: 5 } }])[0];
  if (ackSource.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const oldQueue = withInitialInputFacts(ackSource, [{ ...fact, rev: 1 }]);
  expect(oldQueue.sourceWitness).toEqual({ ...source.inputSource, entryRev: 5 });
  expect(withConfirmedSends([], [oldQueue], [], 'card')).toEqual([]);
  expect(withInitialInputFacts(oldQueue, [{ ...fact, entryId: 'foreign', rev: 9 }])).toBe(oldQueue);
});

it('advances an unbound source body when the actual PATCH restores submitted text', () => {
  const external = { id: '7:0', author: 'you' as const, text: 'An external actual source', atMs: 2,
    queueEntryId: receipt.entryId, inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: null, entryRev: 1 } };
  const witnessed = withInitialInputSources([accepted], [external]);
  const written = acknowledgeQueueMutation(witnessed, { card_id: 'card', entry_id: receipt.entryId, rev: 2, text: accepted.submitted.text });
  expect(written[0]).toMatchObject({ sourceWitness: null, submitted: accepted.submitted });
  const op = written[0]; if (op.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(withConfirmedSends([external], written, [], 'card')).toEqual([initialInputEcho(op)]);
});


it('accepts higher actual source versions and rejects observations below or inconsistent with the immutable receipt revision', () => {
  const operation = initialInputObservationOperation('track', 'card', 'original-key');
  const newerReceipt = { ...receipt, entryRev: 3 };
  for (const invalid of [
    { ...queued(), receipt: newerReceipt, rev: 2 },
    { ...projected, receipt: newerReceipt, entryRev: 2 },
    { ...queued(), receipt: { ...receipt, entryRev: undefined } },
    { ...projected, entryRev: undefined },
  ]) expect(operation.responseSchema.safeParse(invalid).success).toBe(false);
  expect(observationMatchesReceipt({ ...projected, receipt: newerReceipt, entryRev: 3 }, receipt)).toBe(false);
  expect(observationMatchesReceipt({ ...projected, entryRev: 1 }, receipt)).toBe(true);
  const queue = withInitialInputFacts(accepted, [{ kind: 'queue', entryId: receipt.entryId, rev: 0, text: accepted.submitted.text }]);
  const source = { id: '7:0', author: 'you' as const, text: 'Actual external canonical revision one', atMs: 2,
    queueEntryId: receipt.entryId, inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: 'real-turn', entryRev: 1 } };
  const handed = withInitialInputSources([queue], [source])[0];
  if (handed.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(currentInitialInput(handed)).toEqual({ kind: 'observed', rev: 1, text: source.text });
  expect(handed.sourceWitness).toEqual(source.inputSource);
  expect(withConfirmedSends([source], [handed], [], 'card')).toEqual([source]);
  expect(withConfirmedSends([], [handed], [], 'card')).toEqual([]);
});

it.each(['normal', 'delayed PATCH'] as const)('keeps a same-revision Delete stronger than a positive PATCH ACK in %s response order', order => {
  const patch = { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: 'Actual stored revision one' };
  const removed = { ...patch, text: null };
  const writes = order === 'normal' ? [patch, removed] : [removed, patch];
  const ops = acknowledgeQueueMutation(acknowledgeQueueMutation([accepted], writes[0]), writes[1]);
  const op = ops[0]; if (op.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(currentInitialInput(op)).toEqual({ kind: 'write', rev: 1, text: null });
  expect(acknowledgeQueueMutation(ops, { ...patch, rev: 0 })[0]).toBe(op);
  const cached = { entry_id: receipt.entryId, rev: 1, text: patch.text, queued_at_ms: 100 };
  const staleSource = { id: '7:0', author: 'you' as const, text: patch.text, atMs: 2, queueEntryId: receipt.entryId,
    inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: null, entryRev: 1 } };
  expect(withConfirmedSends([staleSource], ops, [], 'card')).toEqual([]);
  expect(initialInputQueueView([cached], op, undefined, 100).entries).toEqual([]);
  const newer = { ...cached, rev: 2, text: 'Actual strictly newer restored input' };
  const restored = withInitialInputFacts(op, [{ kind: 'queue', entryId: receipt.entryId, rev: newer.rev, text: newer.text }]);
  expect(currentInitialInput(restored)).toEqual({ kind: 'observed', rev: 2, text: newer.text });
  expect(initialInputQueueView([newer], restored, undefined, 100).entries).toEqual([newer]);
  expect(withConfirmedSends([], [restored], [], 'card').map(entry => 'text' in entry ? entry.text : entry.author)).toEqual([newer.text]);
  const acknowledgedRestore = acknowledgeQueueMutation(ops, { ...patch, rev: 2, text: newer.text })[0];
  if (acknowledgedRestore.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(currentInitialInput(acknowledgedRestore)).toEqual({ kind: 'write', rev: 2, text: newer.text });
});

it('retains readable current input after an unbound projection withdraws and its same-revision queue drains again', () => {
  const raw = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 2, queueEntryId: receipt.entryId,
    inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: null, entryRev: 0 } };
  const seen = withInitialInputSources([accepted], [raw])[0];
  if (seen.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(withConfirmedSends([raw], [seen], [], 'card')).toEqual([raw]);
  expect(withConfirmedSends([], [seen], [], 'card').map(entry => 'text' in entry ? entry.text : entry.author)).toEqual([raw.text]);
  expect(seen.sourceWitness).toBeNull();
  const rebuffered = withInitialInputFacts(seen, [{ kind: 'queue', entryId: receipt.entryId, rev: 0, text: raw.text }]);
  expect(currentInitialInput(rebuffered)).toEqual({ kind: 'observed', rev: 0, text: raw.text });
  expect(withConfirmedSends([], [rebuffered], [], 'card').map(entry => 'text' in entry ? entry.text : entry.author)).toEqual([raw.text]);
  const bound = { ...raw, id: '8:0', inputSource: { ...raw.inputSource, itemId: 8, turnId: 'actual-provider-turn' } };
  const handed = withInitialInputSources([rebuffered], [raw, bound])[0];
  expect(handed).toMatchObject({ sourceWitness: bound.inputSource });
  expect(withConfirmedSends([], [handed], [], 'card')).toEqual([]);
});

it('places accepted creation input before live replies while ordinary sending words retain their existing tail', () => {
  const reply = { id: 'live-agent', author: 'agent' as const, text: 'Actual live agent reply', atMs: 3 };
  const ordinary = { key: 'ordinary-tail', phase: 'sending' as const, unknown: false, fromComposer: true, replaces: null,
    echo: { id: 'ordinary-tail', author: 'you' as const, text: 'Ordinary tail input', atMs: 4,
      serverHighWaterBefore: 0, queued: false, entryId: null } };
  const expected = [accepted.submitted.text, reply.text, ordinary.echo.text];
  const waiting = view(undefined, 0, { liveReplies: [reply], ops: [accepted, ordinary] });
  expect(waiting.transcript.map(entry => 'text' in entry ? entry.text : entry.author)).toEqual(expected);
  const canonical = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 2, queueEntryId: receipt.entryId,
    inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: 'real-provider-turn', entryRev: 0 } };
  const handed = view(undefined, 1, { serverEntries: [canonical], serverTurns: [canonical], liveReplies: [reply], ops: [accepted, ordinary] });
  expect(handed.transcript.map(entry => 'text' in entry ? entry.text : entry.author)).toEqual(expected);
  const leading = [{ id: 'seed', author: 'system' as const, label: 'Input context', text: 'Actual input context', atMs: 1 },
    { id: 'prior-user', author: 'you' as const, text: 'Actual leading user input', atMs: 1 }];
  const confirmed = { ...ordinary, phase: 'confirmed' as const, queueRevision: null };
  expect(withConfirmedSends([...leading, reply], [accepted, confirmed], [], 'card').map(entry => 'text' in entry ? entry.text : entry.author))
    .toEqual([...leading.map(entry => entry.text), ...expected]);
});

it('accounts only actual final displayed versions and keeps intentional suppression at its authoritative version', () => {
  const row = { entry_id: receipt.entryId, text: accepted.submitted.text, rev: 1, queued_at_ms: 100 };
  const historical = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 1, queueEntryId: receipt.entryId,
    inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: 'real-older-turn', entryRev: 0 } };
  const older = queueDisplayAccounting([historical], [accepted], []);
  expect(older).toEqual({ displayed: [{ id: receipt.entryId, rev: 0 }], suppressed: [] });
  expect(queueEntryIsAccounted(older, row)).toBe(false);
  expect(queueDisplayAccounting([{ ...historical, inputSource: { ...historical.inputSource, entryRev: null } }], [accepted], []).displayed).toEqual([]);
  const revised = acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: receipt.entryId, rev: 2, text: 'Actual revised input' })[0];
  if (revised.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const echo = initialInputEcho(revised);
  expect(echo.inputSource).toBeUndefined();
  const present = queueDisplayAccounting([echo], [revised], []);
  expect(present.displayed).toEqual([{ id: receipt.entryId, rev: 2 }]); expect(queueEntryIsAccounted(present, row)).toBe(true);
  expect(queueDisplayAccounting([], [revised], []).displayed).toEqual([]);
  const deleted = acknowledgeQueueMutation([revised], { card_id: 'card', entry_id: receipt.entryId, rev: 2, text: null })[0];
  if (deleted.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const suppressed = initialInputQueueView([row], deleted, undefined, 100).suppressedRefs;
  expect(suppressed).toEqual([{ id: receipt.entryId, rev: 2 }]);
  const absent = queueDisplayAccounting([], [deleted], suppressed);
  expect(absent.displayed).toEqual([]); expect(queueEntryIsAccounted(absent, row)).toBe(true);
  expect(queueEntryIsAccounted(absent, { ...row, rev: 3 })).toBe(false);
  const ordinary = { key: 'ordinary', phase: 'confirmed' as const, queueRevision: null, fromComposer: true, replaces: null,
    echo: { id: 'ordinary', author: 'you' as const, text: 'Ordinary input', atMs: 3, serverHighWaterBefore: 0, queued: true, entryId: 'ordinary' } };
  expect(queueDisplayAccounting([ordinary.echo], [ordinary], []).displayed).toEqual([]);
  expect(queueDisplayAccounting([ordinary.echo], [{ ...ordinary, queueRevision: 2 }], []).displayed).toEqual([{ id: 'ordinary', rev: 2 }]);
});

it('keeps a later mutable input after actual older bound history and its reply, including when the older bound source goes off page', () => {
  const prior = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 1, queueEntryId: receipt.entryId,
    inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: 'prior-bound-turn', entryRev: 0 } };
  const reply = { id: '8:0', author: 'agent' as const, text: 'Actual older reply', atMs: 2 };
  const outcome = { id: 'old-outcome', author: 'turn' as const, turnId: 'prior-bound-turn', atMs: 2,
    status: 'completed' as const, elapsedMs: null };
  const witnessed = withInitialInputSources([accepted], [prior]);
  const revised = acknowledgeQueueMutation(witnessed, { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: 'Later actual input B' });
  const history = [prior, reply, outcome];
  expect(view(undefined, 1, { serverEntries: history, serverTurns: [prior, reply], ops: revised }).transcript.map(entry => 'text' in entry ? entry.text : entry.author))
    .toEqual([prior.text, reply.text, 'turn', 'Later actual input B']);
  expect(withConfirmedSends(history, revised, [], 'card').map(entry => 'text' in entry ? entry.text : entry.author)).toEqual([prior.text, reply.text, 'turn', 'Later actual input B']);
  expect(view(undefined, 2, { serverEntries: [], serverTurns: [], liveReplies: [reply], ops: revised }).transcript.map(entry => 'text' in entry ? entry.text : entry.author))
    .toEqual([reply.text, 'Later actual input B']);
  expect(withConfirmedSends([reply], revised, [], 'card').map(entry => 'text' in entry ? entry.text : entry.author)).toEqual([reply.text, 'Later actual input B']);
  const seenLate = withInitialInputSources(acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: 'Later actual input B' }), [prior]);
  expect(seenLate[0]).toMatchObject({ sourceWitness: prior.inputSource });
  expect(withConfirmedSends([reply], seenLate, [], 'card').map(entry => 'text' in entry ? entry.text : entry.author)).toEqual([reply.text, 'Later actual input B']);
});


it.each(['live', 'registry'] as const)('keeps the first input while existing Thought normalization changes the %s source length', consumer => {
  const thought = { id: 'thought', author: 'activity' as const, verb: 'Thought', target: null, state: 'done' as const,
    durationMs: null, detail: null, tool: null, atMs: 3 };
  const reply = { id: 'actual-reply', author: 'agent' as const, text: 'Actual later agent speech', atMs: 4 };
  const project = (entries: readonly TranscriptEntry[], live: readonly ConversationTurn[]) => consumer === 'live'
    ? view(undefined, 0, { serverEntries: entries, liveReplies: live }).transcript
    : withConfirmedSends(mergeTranscript(entries, live), [accepted], [], 'card');
  const first = initialInputEcho(accepted);
  expect(project([thought], [])).toEqual([first, thought]);
  expect(project([thought], [reply])).toEqual([first, reply]);
  const activity = { ...thought, id: 'actual-read', verb: 'Read' };
  expect(project([activity], [])).toEqual([first, activity]);
});
