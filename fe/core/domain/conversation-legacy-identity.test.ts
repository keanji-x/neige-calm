import { expect, it } from 'vitest';
import { sha256Hex } from './sha256.js';
import type { HarnessQueueEntryRef } from '../api/generated/wire.js';
import { buildConversationTranscript } from './conversation-display.js';
import { harnessItemSchema, isConversationMessage, type ConversationTurnOutcome } from './conversation.js';
import { matchSendOps, outboxView, withInitialInputSources, queueDisplayAccounting, type AcceptedInitialInput, type SendOp } from './conversation-outbox.js';
import { editedTurnRefill } from './conversation-composer.js';
import { currentResponseMessage, latestUserMessage } from './conversation-actions.js';

function transcript(refs: readonly (HarnessQueueEntryRef | null)[], card = 'card', thread = 'thread', turnId: string | null = 'turn') {
  const item = harnessItemSchema.parse({
    id: 5, worker_session_id: 'session', card_id: card, track_id: 'track', thread_id: thread,
    turn_id: turnId, item_uuid: 'native-item', item_type: 'userMessage', method: 'item/completed',
    params: JSON.stringify({ item: { clientId: 'retained-batch-alias', content: [{ type: 'text', text: 'same words' }] } }),
    created_at_ms: 1, turn_error_text: null,
    input_segments: refs.map(ref => ({ presentation: 'user', text: 'User says:\nsame words', attachments: [],
      origin: { observation: 'user_message', event_id: null, queue_entry: ref } })),
  });
  return buildConversationTranscript([{ cursor: { contract: 'conversation_v1', source: 'historical_raw', card_id: card, position: 5 },
    source: { kind: 'historical_raw', item } }], 'viewer');
}
const send = (entryId: string | null): SendOp & { phase: 'confirmed' } => ({ key: `send-${entryId}`, phase: 'confirmed', queueRevision: null,
  fromComposer: true, replaces: null, echo: { id: `echo-${entryId}`, author: 'you', text: 'same words', atMs: 2,
    serverHighWaterBefore: 0, queued: false, entryId } });

it('preserves each actual segment identity without assigning a historical author', () => {
  const turns = transcript([{ id: 'entry-a', rev: 3 }, { id: 'entry-b', rev: 7 }]).filter(isConversationMessage);
  expect(turns.map(turn => turn.author)).toEqual(['unknown', 'unknown']);
  expect(turns[0]).toMatchObject({ queueEntryId: 'entry-a', inputSource: { cardId: 'card', threadId: 'thread', entryRev: 3 } });
  expect(turns[1]).toMatchObject({ queueEntryId: 'entry-b', inputSource: { cardId: 'card', threadId: 'thread', entryRev: 7 } });
});

it('retires exact acknowledgements for both equal messages in the same native batch', () => {
  const turns = transcript([{ id: 'entry-a', rev: 3 }, { id: 'entry-b', rev: 7 }]).filter(isConversationMessage);
  expect([...matchSendOps(turns, [send('entry-b'), send('entry-a')], [], 'card')])
    .toEqual([['send-entry-b', '5:1'], ['send-entry-a', '5:0']]);
});

it('does not infer an acknowledgement from equal words, the batch alias, or another card', () => {
  const turns = transcript([{ id: 'entry-a', rev: 3 }]).filter(isConversationMessage);
  expect(matchSendOps(turns, [send('retained-batch-alias'), send(null)], [], 'card').size).toBe(0);
  expect(matchSendOps(turns, [send('entry-a')], [], 'different-card').size).toBe(0);
  expect(matchSendOps(transcript([null]).filter(isConversationMessage), [send('entry-a')], [], 'card').size).toBe(0);
});

it('finds the historical user geometry without pretending its author is the viewer', () => {
  const turns = transcript([{ id: 'entry-a', rev: 3 }]);
  const outcome: ConversationTurnOutcome = { id: 'outcome-6', author: 'turn', turnId: 'turn', status: 'completed', atMs: 3, elapsedMs: null };
  expect(editedTurnRefill([...turns, outcome], outcome.id)).toEqual({ text: 'same words', attachments: [] });
  expect(latestUserMessage([...turns, outcome])).toMatchObject({ author: 'unknown', text: 'same words' });
  expect(currentResponseMessage([{ id: 'prior-answer', author: 'agent', text: 'old answer', atMs: 0 }, ...turns], false)).toBeNull();
});

it('does not let an older Unknown version retire a newer acknowledged queue edit', () => {
  const older = transcript([{ id: 'entry-a', rev: 7 }], 'card', 'thread', null);
  const current: SendOp = { ...send('entry-a'), queueRevision: 8, echo: { ...send('entry-a').echo, text: 'Newer current body' } };
  const turns = older.filter(isConversationMessage);
  expect(matchSendOps(turns, [current], [], 'card').size).toBe(0);
  const shown = outboxView({ cardId: 'card', serverEntries: older, serverTurns: turns, queuedEntryIds: new Set(),
    stalled: false, ops: [current], spent: [], landed: { transcript: 1, run: 1 }, liveReplies: [] });
  expect(shown.retire).toEqual([]);
  expect(shown.transcript.map(entry => 'text' in entry ? entry.text : '')).toEqual(['Newer current body']);
});

it('hands initial-input display to a real versioned Unknown source without rewriting attribution', () => {
  const entries = transcript([{ id: 'entry-a', rev: 3 }]);
  const accepted: AcceptedInitialInput = { phase: 'initial-accepted', key: 'actual-create-key', trackId: 'track',
    submitted: { text: 'same words', atMs: 1 }, receipt: { version: 'initial-input-v1', entryId: 'entry-a', entryRev: 0,
      queuedAtMs: 1, textSha256: sha256Hex('same words'), initialRuntimeId: 'runtime' },
    localWrite: null, inputFact: null, sourceWitness: null };
  const ops = withInitialInputSources([accepted], entries);
  const shown = outboxView({ cardId: 'card', serverEntries: entries, serverTurns: entries.filter(isConversationMessage),
    queuedEntryIds: new Set(), stalled: false, ops, spent: [], landed: { transcript: 1, run: 1 }, liveReplies: [] });
  expect(shown.transcript).toEqual(entries);
  expect(shown.checkingInitialInput).toBe(false);
  expect(entries[0].author).toBe('unknown');
  expect(queueDisplayAccounting(entries, ops, []).displayed).toEqual([{ id: 'entry-a', rev: 3 }]);
  expect(ops[0].phase === 'initial-accepted' && ops[0].sourceWitness).toMatchObject({ cardId: 'card', threadId: 'thread', entryRev: 3 });
});
