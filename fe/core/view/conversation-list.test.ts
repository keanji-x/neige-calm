import { expect, it } from 'vitest';
import type { Conversation } from '../domain/conversation.js';
import { deriveConversationList } from './conversation-list.js';

const conversation = (id: string): Conversation => ({ id, provider: 'codex', trackId: 'work', title: id,
  kind: 'track-assistant', state: 'running', updatedAt: 1 });

it('keeps questions, failures and unread distinct from idle session state', () => {
  const rows = deriveConversationList({ conversations: ['input', 'failed', 'unread', 'idle'].map(conversation),
    cards: { input: 'input', failed: 'failed' }, unreadIds: new Set(['unread']) });
  expect(rows.map(row => [row.name, row.activity])).toEqual([
    ['input', 'attention'], ['failed', 'failed'], ['unread', 'unread'], ['idle', 'quiet'],
  ]);
});

it('uses the active drawer echo and the same failure precedence as the ordinary list', () => {
  const rows = deriveConversationList({ conversations: [conversation('one')], cards: { one: 'working' },
    local: { id: 'one', working: true, stalled: true } });
  expect(rows[0]).toMatchObject({ activity: 'failed' });
});


it('uses explicit pending-question ownership without changing other conversations or failure precedence', () => {
  const rows = deriveConversationList({ conversations: ['owner', 'other', 'broken'].map(conversation),
    cards: { owner: 'working', other: 'working', broken: 'failed' }, unreadIds: new Set(['owner']),
    awaitingInputIds: new Set(['owner', 'broken']),
  });
  expect(rows.map(row => [row.name, row.activity])).toEqual([
    ['owner', 'attention'], ['other', 'working'], ['broken', 'failed'],
  ]);
});
