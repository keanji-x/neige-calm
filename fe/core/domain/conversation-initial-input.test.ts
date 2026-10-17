import { describe, expect, it } from 'vitest';
import type { ConversationInitialInputObservation } from '../api/generated/wire.js';
import { initialInputObservationOperation, receiptMatchesSubmission } from './conversation-initial-input.js';
import { outboxView, initialInputQueueView, initialInputEcho, matchSendOps, acknowledgeQueueMutation, withInitialInputSources, initialInputNeedsObservation, withConfirmedSends, initialInputDisplaced, type AcceptedInitialInput, type ObservedInitialInput } from './conversation-outbox.js';
import { sha256Hex } from './sha256.js';

const receipt = Object.freeze({ version: 'initial-input-v1' as const, entryId: 'first-entry', queuedAtMs: 100,
  textSha256: sha256Hex('first words'), initialRuntimeId: 'runtime' });
const accepted: AcceptedInitialInput = Object.freeze({ phase: 'initial-accepted', key: 'original-key', trackId: 'track',
  submitted: Object.freeze({ text: 'first words', atMs: 2 }), receipt, localWrite: null, sourceWitness: null });
const observed = (observation: ConversationInitialInputObservation, afterRead = 10): ObservedInitialInput => ({ observation, afterRead });
const queued = (text = 'first words', rev = 0): ConversationInitialInputObservation => ({ kind: 'queued', receipt,
  runtimeId: 'runtime', entryId: receipt.entryId, rev, text, textSha256: sha256Hex(text) });
const projected: ConversationInitialInputObservation = { kind: 'projected', receipt, runtimeId: 'runtime', itemId: 70, segmentIndex: 2 };
const view = (observation?: ObservedInitialInput, transcriptRead = 0,
  extra: Partial<Parameters<typeof outboxView>[0]> = {}) => outboxView({
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
      const prior = initialInputQueueView([entry], accepted, observed(observation), 9);
      expect(prior.entries).toEqual([]);
      expect([...prior.hiddenIds]).toEqual([receipt.entryId]);
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
    const carrier = { id: '70:2', author: 'you' as const, text: 'combined words', atMs: 3, queueEntryId: receipt.entryId, inputSource: { itemId: 70, segmentIndex: 2, turnId: 'accepted-turn' } };
    const extra = { serverEntries: [carrier], serverTurns: [carrier], spent: ['70:2'] };
    expect(view(undefined, 0, extra).retire).toEqual([]);
    expect(view(undefined, 0, extra).transcript).toEqual([carrier]);
    const older = { ...carrier, inputSource: { itemId: 69, segmentIndex: 2, turnId: 'accepted-turn' } };
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
  const carrier = { id: '70:2', author: 'you' as const, text: 'first words', atMs: 2, queueEntryId: receipt.entryId };
  expect(withConfirmedSends([carrier], ops, [])).toEqual([]);
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
  expect(withConfirmedSends([], same, [])).toMatchObject([{ text: accepted.submitted.text }]);
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
    queueEntryId: receipt.entryId, inputSource: { itemId: 7, segmentIndex: 0, turnId: null } };
  const bound = { ...old, id: '8:0', inputSource: { itemId: 8, segmentIndex: 0, turnId: 'real-provider-turn' } };
  const revised = { ...old, id: '9:0', text: 'Actual acknowledged revised words', inputSource: { itemId: 9, segmentIndex: 0, turnId: null } };
  const result = { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: revised.text };
  const ops = acknowledgeQueueMutation([accepted], result);
  const entries = [old, bound, revised];
  const shown = view(observed(queued()), 1000, { ops, serverEntries: entries, serverTurns: entries });
  expect(shown.transcript).toEqual([bound, revised]);
  expect(shown.retire).toEqual([]);
  expect(withConfirmedSends(entries, ops, [])).toEqual([bound, revised]);
});


it('never lets the receipt-identified accepted display certify its own raw canonical carrier', () => {
  const display = initialInputEcho(accepted);
  expect(display.queueEntryId).toBe(receipt.entryId);
  expect(display.inputSource).toBeUndefined();
  expect(matchSendOps([display], [accepted], []).size).toBe(0);
});


it('lets a same-body PATCH receipt outrank an older changed queued inspection without retiring original acceptance', () => {
  const ops = acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: receipt.entryId, rev: 2, text: accepted.submitted.text });
  const op = ops[0];
  if (op.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const old = observed(queued('Older changed body', 1), 30);
  expect(initialInputDisplaced(op, old)).toBe(false);
  expect(view(old, 1000, { ops }).retire).toEqual([]);
});


it('transfers display ownership only from an admitted exact raw source and retains one positive witness across page misses and later ACKs', () => {
  const source = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 2,
    queueEntryId: receipt.entryId, inputSource: { itemId: 7, segmentIndex: 0, turnId: null } };
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
  expect(withConfirmedSends([], handed, [])).toEqual([]);
  const removed = acknowledgeQueueMutation(handed, { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: null });
  expect(removed[0]).toMatchObject({ sourceWitness: source.inputSource, localWrite: { text: null } });
  expect(view(undefined, 1000, { ops: removed, serverEntries: [source], serverTurns: [source] }).transcript).toEqual([]);
  expect(initialInputNeedsObservation(accepted)).toBe(true);
});


it('uses actual PATCH body and revision only for an existing cached queue row, letting an actual newer revision or queue absence win', () => {
  const old = { entry_id: receipt.entryId, text: accepted.submitted.text, rev: 0, queued_at_ms: receipt.queuedAtMs };
  const saved = { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: 'Actual stored revised body' };
  const op = acknowledgeQueueMutation([accepted], saved)[0];
  if (op.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  const queuedView = initialInputQueueView([old], op, observed(queued()), 0);
  expect(queuedView.entries).toEqual([{ ...old, text: saved.text, rev: saved.rev }]);
  expect([...queuedView.hiddenIds]).toEqual([]);
  expect(initialInputQueueView([], op, observed(queued()), 1000).entries).toEqual([]);
  const newer = { ...old, text: 'Actual later revision', rev: 2 };
  expect(initialInputQueueView([newer], op, observed(queued()), 1000).entries).toEqual([newer]);
  expect(initialInputEcho(op)).toMatchObject({ text: saved.text, queueEntryId: receipt.entryId });
  expect(initialInputEcho(op).inputSource).toBeUndefined();
  expect(withConfirmedSends([], [op], [])).toEqual([initialInputEcho(op)]);
});

it('hands off only the current actual ACK body and ends creation observation through same-body and restored-original edits', () => {
  const originalSource = { id: '7:0', author: 'you' as const, text: accepted.submitted.text, atMs: 2,
    queueEntryId: receipt.entryId, inputSource: { itemId: 7, segmentIndex: 0, turnId: 'old-provider-turn' } };
  const witnessed = withInitialInputSources([accepted], [originalSource]);
  const same = acknowledgeQueueMutation(witnessed, { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: accepted.submitted.text });
  expect(same[0]).toMatchObject({ sourceWitness: originalSource.inputSource });
  const plainSame = acknowledgeQueueMutation([accepted], { card_id: 'card', entry_id: receipt.entryId, rev: 1, text: accepted.submitted.text })[0];
  if (plainSame.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(initialInputNeedsObservation(plainSame)).toBe(false);
  const edited = acknowledgeQueueMutation(same, { card_id: 'card', entry_id: receipt.entryId, rev: 2, text: 'Actual revised body' });
  expect(edited[0]).toMatchObject({ sourceWitness: null });
  expect(withInitialInputSources(edited, [originalSource])).toBe(edited);
  expect(withConfirmedSends([originalSource], edited, []).map(entry => 'text' in entry ? entry.text : entry.author)).toEqual([accepted.submitted.text, 'Actual revised body']);
  const editedSource = { ...originalSource, id: '8:0', text: 'Actual revised body', inputSource: { itemId: 8, segmentIndex: 0, turnId: null } };
  const handed = withInitialInputSources(edited, [originalSource, editedSource]);
  expect(handed[0]).toMatchObject({ sourceWitness: editedSource.inputSource });
  expect(withConfirmedSends([], handed, [])).toEqual([]);
  const restored = acknowledgeQueueMutation(handed, { card_id: 'card', entry_id: receipt.entryId, rev: 3, text: accepted.submitted.text })[0];
  if (restored.phase !== 'initial-accepted') throw new Error('Expected acceptance');
  expect(restored.sourceWitness).toBeNull(); expect(initialInputNeedsObservation(restored)).toBe(false);
  expect(initialInputEcho(restored).text).toBe(accepted.submitted.text);
});
