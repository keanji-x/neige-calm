// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { sentMentionParts } from '../../../../../core/domain/mentions.ts';
import { ChatThread } from './public.tsx';

// Count the real renderer's work; preserve the authoritative mention parser.
vi.mock(import('../../../../../core/domain/mentions.ts'), async (original) => {
  const actual = await original();
  return { ...actual, sentMentionParts: vi.fn(actual.sentMentionParts) };
});
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const conversation: Conversation = {
  id: 'card', trackId: 'track', title: 'Measured messages', kind: 'codex', state: 'idle', updatedAt: 3,
};
const question: ConversationTurn = { id: 'you', author: 'you', text: 'First question', atMs: 1 };
const stored: ConversationTurn = { id: 'stored', author: 'agent', text: 'Stored response', atMs: 2 };
const live: ConversationTurn = { id: 'live', author: 'agent', text: 'Live response', atMs: 3 };

it('leaves stored message bodies untouched when live text grows and transcript objects are replaced', () => {
  const view = render(<ChatThread conversation={conversation} turns={[question, stored, live]}
    cards={{}} stalled={false} canContinue={false} />);
  const existing = screen.getByText('First question');
  expect(sentMentionParts).toHaveBeenCalledWith('First question');
  vi.mocked(sentMentionParts).mockClear();
  view.rerender(<ChatThread conversation={conversation}
    turns={[{ ...question }, { ...stored }, { ...live, text: 'Live response grows' }]}
    cards={{}} stalled={false} canContinue={false} />);
  expect(sentMentionParts).not.toHaveBeenCalled();
  expect(screen.getByText('First question')).toBe(existing);
  expect(screen.getByText('Live response grows')).toBeTruthy();
  expect(document.querySelectorAll('[data-nc-turn="you"], [data-nc-turn="agent"]')).toHaveLength(3);
});

it('updates changed stored words under the same message identity', () => {
  const view = render(<ChatThread conversation={conversation} turns={[question, stored]}
    cards={{}} stalled={false} canContinue={false} />);
  const existing = screen.getByText('First question');
  vi.mocked(sentMentionParts).mockClear();
  view.rerender(<ChatThread conversation={conversation} turns={[{ ...question, text: 'Second question' }, stored]}
    cards={{}} stalled={false} canContinue={false} />);
  expect(sentMentionParts).toHaveBeenCalledWith('Second question');
  expect(screen.getByText('Second question')).toBe(existing);
  expect(screen.queryByText('First question')).toBeNull();
});
