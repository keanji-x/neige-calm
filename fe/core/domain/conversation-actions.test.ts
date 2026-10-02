import { expect, it } from 'vitest';
import { currentResponseMessage, latestUserMessage } from './conversation-actions.js';
import type { OptimisticConversationTurn } from './conversation.js';

it('copies verbatim Markdown from the current response only', () => {
  const reply = { id: 'reply', author: 'agent' as const, text: 'Answer\n\n```ts\nx()\n```', atMs: 2 };
  const end = { id: 'end', author: 'turn' as const, turnId: 'turn', status: 'completed' as const, atMs: 3 };
  expect(currentResponseMessage([reply, end], true)).toBe(reply);
  expect(currentResponseMessage([reply, end], false)).toBeNull();
  expect(currentResponseMessage([reply, { id: 'new', author: 'you', text: 'New prompt', atMs: 4 }], false)).toBeNull();
  expect(currentResponseMessage([{ ...end, id: 'old-end' }, reply], false)).toBe(reply);
});

it('keeps queued messages out of current-response copy and delivered-message editing', () => {
  const delivered = { id: 'user', author: 'you' as const, text: 'Original prompt', atMs: 1 };
  const reply = { id: 'reply', author: 'agent' as const, text: 'Current answer', atMs: 2 };
  const queued: OptimisticConversationTurn = { id: 'queued', author: 'you', text: 'Queued prompt', atMs: 3,
    serverHighWaterBefore: 2, queued: true, entryId: 'queued-entry' };
  expect(currentResponseMessage([delivered, reply, queued], false)).toBe(reply);
  expect(latestUserMessage([delivered, reply, queued])).toBe(delivered);
});

it('does not regenerate a previous prompt across a system or turn boundary', () => {
  const user = { id: 'u', author: 'you' as const, text: 'Older prompt', atMs: 1 };
  const end = { id: 'end', author: 'turn' as const, turnId: 't', status: 'completed' as const, atMs: 2 };
  expect(latestUserMessage([user, end], true)).toBe(user);
  expect(latestUserMessage([user, end], false)).toBeNull();
  expect(latestUserMessage([user, { id: 'wake', author: 'system', label: 'Wake', text: 'Automatic turn', atMs: 3 },
    { id: 'reply', author: 'agent', text: 'Automatic response', atMs: 4 }])).toBeNull();
  expect(latestUserMessage([user, end, { id: 'reply', author: 'agent', text: 'Automatic response', atMs: 3 }])).toBeNull();
});
