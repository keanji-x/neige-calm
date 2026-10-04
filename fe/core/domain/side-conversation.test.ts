import { describe, expect, it } from 'vitest';
import { sideConversationSnapshot, sideQuestion } from './side-conversation.js';
import type { ConversationTurn, OptimisticConversationTurn, TranscriptEntry } from './conversation.js';

const speech = (id: string, author: 'you' | 'agent', text: string): ConversationTurn => ({ id, author, text, atMs: 1 });
describe('discussion context', () => {
  it('carries delivered speech while excluding system, tools, images and optimistic prompts', () => {
    const optimistic: OptimisticConversationTurn = { ...speech('echo', 'you', 'unsent'), serverHighWaterBefore: 1, queued: false, entryId: null };
    const entries: TranscriptEntry[] = [speech('user', 'you', 'question'),
      { id: 'system', author: 'system', label: 'Context', text: 'hidden', atMs: 1 },
      { id: 'tool', author: 'activity', verb: 'Read', target: '/secret', state: 'done', durationMs: 1, detail: 'secret', tool: 'Read', atMs: 1 },
      optimistic,
      { ...speech('reply', 'agent', 'answer'), attachments: [{ id: 'image', url: '/secret/image', contentType: 'image/png', size: 1 }] },
    ];
    expect(sideConversationSnapshot('parent', entries)).toEqual({ source_card_id: 'parent', context: 'User: question\n\nAssistant: answer' });
  });
  it('bounds Unicode context and discloses the omitted beginning', () => {
    const side = sideConversationSnapshot('parent', [speech('reply', 'agent', '😀'.repeat(13_000) + 'latest')]);
    expect(Array.from(side.context)).toHaveLength(12_000);
    expect(side.context).toMatch(/^\[Earlier text omitted\]/);
    expect(side.context.endsWith('latest')).toBe(true);
  });
  it('recognizes only the complete side command', () => {
    expect(sideQuestion('/side')).toBe('');
    expect(sideQuestion('/side why?')).toBe('why?');
    expect(sideQuestion('/sideways')).toBeNull();
    expect(sideQuestion('explain /side')).toBeNull();
  });
});
