import { expect, it } from 'vitest';
import { buildConversationTranscript } from './conversation-display.js';
import { harnessItemSchema, isConversationMessage, type ConversationTurnOutcome } from './conversation.js';
import { matchSendOps, type SendOp } from './conversation-outbox.js';
import { editedTurnRefill } from './conversation-composer.js';
import { currentResponseMessage, latestUserMessage } from './conversation-actions.js';

function transcript(ids: readonly (string | null)[], card = 'card', thread = 'thread') {
  const item = harnessItemSchema.parse({
    id: 5, worker_session_id: 'session', card_id: card, track_id: 'track', thread_id: thread,
    turn_id: 'turn', item_uuid: 'native-item', item_type: 'userMessage', method: 'item/completed',
    params: JSON.stringify({ item: { clientId: 'retained-batch-alias', content: [{ type: 'text', text: 'same words' }] } }),
    created_at_ms: 1, turn_error_text: null,
    input_segments: ids.map(id => ({ presentation: 'user', text: 'User says:\nsame words', attachments: [],
      origin: { observation: 'user_message', event_id: null, ...(id === null ? {} : { queue_entry_id: id }) } })),
  });
  return buildConversationTranscript([{ cursor: { contract: 'conversation_v1', source: 'historical_raw', card_id: card, position: 5 },
    source: { kind: 'historical_raw', item } }], 'viewer');
}
const send = (entryId: string | null): SendOp => ({ key: `send-${entryId}`, phase: 'confirmed', queueRevision: null,
  fromComposer: true, replaces: null, echo: { id: `echo-${entryId}`, author: 'you', text: 'same words', atMs: 2,
    serverHighWaterBefore: 0, queued: false, entryId } });

it('preserves each actual segment identity without assigning a historical author', () => {
  const turns = transcript(['entry-a', 'entry-b']).filter(isConversationMessage);
  expect(turns.map(turn => turn.author)).toEqual(['unknown', 'unknown']);
  expect(turns[0]).toMatchObject({ legacyInput: { cardId: 'card', threadId: 'thread', entryId: 'entry-a' } });
  expect(turns[1]).toMatchObject({ legacyInput: { cardId: 'card', threadId: 'thread', entryId: 'entry-b' } });
});

it('retires exact acknowledgements for both equal messages in the same native batch', () => {
  const turns = transcript(['entry-a', 'entry-b']).filter(isConversationMessage);
  expect([...matchSendOps(turns, [send('entry-b'), send('entry-a')], [], 'card')])
    .toEqual([['send-entry-b', '5:1'], ['send-entry-a', '5:0']]);
});

it('does not infer an acknowledgement from equal words, the batch alias, or another card', () => {
  const turns = transcript(['entry-a']).filter(isConversationMessage);
  expect(matchSendOps(turns, [send('retained-batch-alias'), send(null)], [], 'card').size).toBe(0);
  expect(matchSendOps(turns, [send('entry-a')], [], 'different-card').size).toBe(0);
  expect(matchSendOps(transcript([null]).filter(isConversationMessage), [send('entry-a')], [], 'card').size).toBe(0);
});

it('finds the historical user geometry without pretending its author is the viewer', () => {
  const turns = transcript(['entry-a']);
  const outcome: ConversationTurnOutcome = { id: 'outcome-6', author: 'turn', turnId: 'turn', status: 'completed', atMs: 3, elapsedMs: null };
  expect(editedTurnRefill([...turns, outcome], outcome.id)).toEqual({ text: 'same words', attachments: [] });
  expect(latestUserMessage([...turns, outcome])).toMatchObject({ author: 'unknown', text: 'same words' });
  expect(currentResponseMessage([{ id: 'prior-answer', author: 'agent', text: 'old answer', atMs: 0 }, ...turns], false)).toBeNull();
});
