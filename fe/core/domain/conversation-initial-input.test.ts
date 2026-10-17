import { describe, expect, it } from 'vitest';
import type { ConversationInitialInputObservation } from '../api/generated/wire.js';
import { initialInputObservationOperation, receiptMatchesSubmission, initialInputQueueView, type AcceptedInitialInput, type ObservedInitialInput } from './conversation-initial-input.js';
import { outboxView } from './conversation-outbox.js';
import { sha256Hex } from './sha256.js';

const receipt = Object.freeze({ version: 'initial-input-v1' as const, entryId: 'first-entry', queuedAtMs: 100,
  textSha256: sha256Hex('first words'), initialRuntimeId: 'runtime' });
const accepted: AcceptedInitialInput = Object.freeze({ phase: 'initial-accepted', key: 'original-key', trackId: 'track',
  submitted: Object.freeze({ text: 'first words', atMs: 2 }), receipt });
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
    expect(shown.transcript).toEqual([{ id: 'initial-original-key', author: 'you', text: 'first words', atMs: 2 }]);
    expect(shown.sending).toBe(false);
    expect(shown.failed).toBeNull();
    expect(shown.confirmed).toEqual([]);
    expect(shown.checkingInitialInput).toBe(true);
  });

  it('uses the real pending region for a coherently queued entry and permits ordinary input', () => {
    const shown = view(observed(queued()), 1000, { queuedEntryIds: new Set([receipt.entryId]) });
    expect(shown.transcript).toEqual([]);
    expect(shown.retire).toEqual([]);
    expect(shown.blocked).toBe(false);
    expect(shown.checkingInitialInput).toBe(false);
  });

  it('hides changed or absent original words immediately and retains nondrawing bookkeeping until both fresh sources land', () => {
    for (const observation of [queued('edited words', 1), { kind: 'absent', receipt, runtimeId: 'runtime' } as const]) {
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
    for (const observation of [queued('edited words', 1), { kind: 'absent', receipt, runtimeId: 'runtime' } as const, projected]) {
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
    const carrier = { id: '70:2', author: 'you' as const, text: 'combined words', atMs: 3, queueEntryId: receipt.entryId, inputSource: { itemId: 70, segmentIndex: 2 } };
    const extra = { serverEntries: [carrier], serverTurns: [carrier], spent: ['70:2'] };
    expect(view(undefined, 0, extra).retire).toEqual([]);
    expect(view(undefined, 0, extra).transcript).toEqual([carrier]);
    const older = { ...carrier, inputSource: { itemId: 69, segmentIndex: 2 } };
    expect(view(observed(projected), 0, { serverEntries: [older], serverTurns: [older] }).retire).toEqual([]);
    expect(view(observed(projected), 11, { serverEntries: [older], serverTurns: [older] }).retire).toEqual([{ key: accepted.key, row: null }]);
    const shown = view(observed(projected), 0, extra);
    expect(shown.retire).toEqual([{ key: accepted.key, row: '70:2' }]);
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
      { kind: 'absent', runtimeId: 'runtime' },
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
