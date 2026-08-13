import { describe, expect, it } from 'vitest';

import type { HarnessItem } from '../api/generated/wire.js';
import {
  CONVERSATION_NAME_MAX, conversationName, conversationNameFrom, harnessItemToTurn,
  reconcileUserEchoes, type Conversation, type ConversationTurn,
} from './conversation.js';

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', waveId: 'w1', waveTitle: 'Ship the rewrite', title: null,
    kind: 'codex', state: 'idle', updatedAt: 0, turns: 0,
    ...overrides,
  };
}

describe('conversationName', () => {
  it('prefers the conversation\'s own name', () => {
    expect(conversationName(conversation({ title: 'Why the resolver drops a hop' })))
      .toBe('Why the resolver drops a hop');
  });

  it('falls back to the kind, never to the wave', () => {
    const nameless = conversation({ waveTitle: 'Ship the rewrite' });
    expect(conversationName(nameless)).toBe('Codex');
    expect(conversationName(nameless)).not.toBe('Ship the rewrite');
  });
});

describe('reconcileUserEchoes', () => {
  const turn = (id: string, text: string): ConversationTurn => ({ id, author: 'you', text, atMs: 1 });

  it('lets one server row consume only one of two identical echoes', () => {
    expect(reconcileUserEchoes(
      [turn('server-1', 'same')],
      [turn('echo-1', 'same'), turn('echo-2', 'same')],
    ).map((entry) => entry.id)).toEqual(['echo-2']);
  });

  it('does not reconcile against server rows outside the bounded lookback', () => {
    const rows = [turn('old', 'same'), ...Array.from({ length: 50 }, (_, index) => turn(`recent-${index}`, `text-${index}`))];
    expect(reconcileUserEchoes(rows, [turn('echo', 'same')])).toHaveLength(1);
  });
});

describe('conversationNameFrom', () => {
  it('takes the first line, not the first paragraph', () => {
    expect(conversationNameFrom('Fix the resolver\n\nHere is the stack trace:\n  at walk()'))
      .toBe('Fix the resolver');
  });

  it('truncates to one name-length, ellipsis included in the budget', () => {
    const name = conversationNameFrom('x'.repeat(200));
    expect(name).toHaveLength(CONVERSATION_NAME_MAX);
    expect(name?.endsWith('…')).toBe(true);
  });

  it('leaves a name that already fits exactly alone', () => {
    const exact = 'y'.repeat(CONVERSATION_NAME_MAX);
    expect(conversationNameFrom(exact)).toBe(exact);
  });

  it.each([['empty', ''], ['whitespace', '   \n  ']])('has no name for a %s message', (_label, text) => {
    expect(conversationNameFrom(text)).toBeNull();
  });
});

function item(overrides: Partial<HarnessItem> = {}): HarnessItem {
  return {
    id: 7, runtime_id: 'runtime', card_id: 'card', wave_id: 'wave', thread_id: 'thread',
    turn_id: 'turn', item_uuid: 'item', item_type: 'agent_message', method: 'item/completed',
    params: JSON.stringify({ completedAtMs: 99, item: { text: 'answer' } }), created_at_ms: 50,
    ...overrides,
  };
}

describe('harnessItemToTurn', () => {
  it('maps completed agent messages', () => {
    expect(harnessItemToTurn(item())).toEqual({ id: '7', author: 'agent', text: 'answer', atMs: 99 });
  });

  it('strips the injected wave diff and user marker', () => {
    const text = '## Wave state changes since your last turn\nchanged\n\n---\n\nUser says:\nhello';
    expect(harnessItemToTurn(item({
      item_type: 'user_message', params: JSON.stringify({ item: { content: [{ type: 'text', text }] } }),
    }))).toMatchObject({ author: 'you', text: 'hello' });
  });

  it('drops incomplete and unsupported entries', () => {
    expect(harnessItemToTurn(item({ method: 'item/started' }))).toBeNull();
    expect(harnessItemToTurn(item({ item_type: 'command_execution' }))).toBeNull();
    expect(harnessItemToTurn(item({ params: '{broken' }))).toBeNull();
  });
});
