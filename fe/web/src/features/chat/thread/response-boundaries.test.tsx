import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import type { ConversationDisplayItem } from '../../../../../core/api/generated/wire.ts';
import type { Conversation, TranscriptEntry } from '../../../../../core/domain/conversation.ts';
import { buildConversationTranscript } from '../../../../../core/domain/conversation-display.ts';
import { liveReplyTurns, reconcileLiveReplies } from '../../../../../core/domain/conversation-live.ts';
import { ChatThread } from './public.tsx';

afterEach(cleanup);

const conversation: Conversation = { provider: 'codex', id: 'card', trackId: 'track', title: 'Replies',
  kind: 'codex', state: 'idle', updatedAt: 1 };
function human(id: string, position: number): ConversationDisplayItem {
  return { cursor: { contract: 'conversation_v1', source: 'presentation', card_id: 'card', position },
    source: { kind: 'human', message_id: id, revision: 1, author: { user_id: 'viewer', display_name: 'Viewer' },
      text: id, attachments: [], created_at_ms: position } };
}
function native(id: string, turn: string | null, position: number, thread = 'thread', tool = false): ConversationDisplayItem {
  return { cursor: { contract: 'conversation_v1', source: 'presentation', card_id: 'card', position }, source: {
    kind: 'native', item: { id: position, card_id: 'card', track_id: 'track', worker_session_id: 'runtime',
      thread_id: thread, turn_id: turn, item_uuid: id, item_type: tool ? 'commandExecution' : 'agentMessage',
      method: 'item/completed', params: JSON.stringify({ item: tool ? { command: id, exitCode: 0 } : { text: id } }),
      created_at_ms: position, turn_error_text: null },
  } };
}
function thread(entries: readonly TranscriptEntry[]) {
  return <ChatThread conversation={conversation} turns={entries} hasPendingMessages={false}
    stalled={false} canContinue={false} cards={{}} />;
}

it.each([false, true])('separates replies to an early queued message (first reply already started: %s)', started => {
  const items = [human('Question A', 1), ...(started ? [native('A starts', 'A', 2)] : []),
    human('Question B', 3), native('A finishes', 'A', 4), native('B answers', 'B', 5)];
  const entries = buildConversationTranscript(items, 'viewer');
  const { container } = render(thread(entries));
  const separator = screen.getByRole('separator', { name: '新一轮回复' });
  expect(screen.getByText('A finishes').compareDocumentPosition(separator) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(separator.compareDocumentPosition(screen.getByText('B answers')) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(container.querySelectorAll('[data-nc-turn="agent"]')).toHaveLength(started ? 3 : 2);
});

it('retains the response boundary when a streamed reply becomes durable history', () => {
  const items = [human('Question A', 1), human('Question B', 2), native('A answers', 'A', 3)];
  const entries = buildConversationTranscript(items, 'viewer');
  const copies = reconcileLiveReplies([], { kind: 'native-poll', atMs: 4, reply: { contract: 'conversation_v1',
    turn: { thread_id: 'thread', turn_id: 'B', items: [{ item_id: 'B-item', text: 'B answers' }] } } });
  const { rerender } = render(thread([...entries, ...liveReplyTurns(copies)]));
  expect(screen.getAllByRole('separator', { name: '新一轮回复' })).toHaveLength(1);
  rerender(thread(buildConversationTranscript([...items, native('B answers', 'B', 4)], 'viewer')));
  expect(screen.getAllByRole('separator', { name: '新一轮回复' })).toHaveLength(1);
  expect(screen.getAllByText('B answers')).toHaveLength(1);
});

it('keeps same-turn messages together when a steer appears between them', () => {
  render(thread(buildConversationTranscript([human('Question A', 1), native('A starts', 'A', 2),
    human('Additional instruction', 3), native('A finishes', 'A', 4)], 'viewer')));
  expect(screen.queryByRole('separator', { name: '新一轮回复' })).toBeNull();
});

it('does not duplicate the boundary already formed by the next user message', () => {
  render(thread(buildConversationTranscript([human('Question A', 1), native('A answers', 'A', 2),
    human('Question B', 3), native('B answers', 'B', 4)], 'viewer')));
  expect(screen.queryByRole('separator', { name: '新一轮回复' })).toBeNull();
});

it('scopes repeated native turn IDs by their original thread', () => {
  render(thread(buildConversationTranscript([native('First thread', 'same', 1, 'first'),
    native('Second thread', 'same', 2, 'second')], 'viewer')));
  expect(screen.getAllByRole('separator', { name: '新一轮回复' })).toHaveLength(1);
});

it('does not invent boundaries for replies with unknown native turns', () => {
  render(thread(buildConversationTranscript([native('First unknown', null, 1), native('Second unknown', null, 2)], 'viewer')));
  expect(screen.queryByRole('separator', { name: '新一轮回复' })).toBeNull();
});

it('keeps tools from successive native turns on opposite sides of the boundary', () => {
  render(thread(buildConversationTranscript([native('first command', 'A', 1, 'thread', true),
    native('second command', 'B', 2, 'thread', true)], 'viewer')));
  expect(screen.getByRole('separator', { name: '新一轮回复' })).toBeTruthy();
  expect(screen.queryByRole('group', { name: '2 tool calls' })).toBeNull();
});
