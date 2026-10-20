import { describe, expect, it } from 'vitest';
import { NotSentError } from './failure-class.js';
import {
  PLANNER_HISTORY_KIND, parsePlannerHistoryKey, plannerConversationsOperation, plannerHistoryKey, plannerHistoryRows,
  plannerNewConversationFailureText, startPlannerConversationOperation,
} from './planner-conversation.js';

const http = (status: number, message: string) => ({ kind: 'http' as const, status, code: 'conflict', message, body: null });

describe('planner conversation operations (#2774)', () => {
  it('starts a conversation under the key it is given and decodes the replayable answer', () => {
    const operation = startPlannerConversationOperation('card/1', 'key-a');
    expect(operation).toMatchObject({ method: 'POST', path: '/api/cards/card%2F1/planner/conversations' });
    expect(operation.headers).toEqual({ 'Idempotency-Key': 'key-a' });
    expect(operation.responseSchema.parse({ card_id: 'card/1', conversation: 2, new_thread_id: 't' }))
      .toEqual({ card_id: 'card/1', conversation: 2, new_thread_id: 't' });
    expect(() => operation.responseSchema.parse({ card_id: 'card/1', new_thread_id: 't' })).toThrow();
  });

  it('lists earlier conversations and rejects a row without its required times', () => {
    const operation = plannerConversationsOperation('card');
    expect(operation).toMatchObject({ method: 'GET', path: '/api/cards/card/planner/conversations' });
    expect(operation.responseSchema.parse([{ ordinal: 0, started_at_ms: 1, ended_at_ms: 2 }])).toHaveLength(1);
    expect(() => operation.responseSchema.parse([{ ordinal: 0, started_at_ms: 1 }])).toThrow();
  });

  it('says the server reason for a refusal and a fixed line for an unconfirmed outcome', () => {
    expect(plannerNewConversationFailureText(http(409, 'The Planner is working.'))).toBe('The Planner is working.');
    expect(plannerNewConversationFailureText(http(503, 'Still settling.'))).toBe('Still settling.');
    expect(plannerNewConversationFailureText(http(409, ''))).toBe('The new conversation was not started.');
    expect(plannerNewConversationFailureText(new NotSentError())).toBe('The new conversation was not started.');
    expect(plannerNewConversationFailureText(http(500, 'start failed'))).toBe('Starting a new conversation is unconfirmed.');
    expect(plannerNewConversationFailureText({ kind: 'transport', message: 'lost' })).toBe('Starting a new conversation is unconfirmed.');
  });
});

describe('earlier Planner conversation rows', () => {
  it('keys a row by card and ordinal and parses only that shape', () => {
    expect(plannerHistoryKey('card', 3)).toBe('card#3');
    expect(parsePlannerHistoryKey('card#3')).toEqual({ cardId: 'card', ordinal: 3 });
    expect(parsePlannerHistoryKey('card#0')).toEqual({ cardId: 'card', ordinal: 0 });
    for (const id of ['card', 'card#', '#1', 'card#01', 'card#-1', 'a#b#1']) expect(parsePlannerHistoryKey(id)).toBeNull();
  });

  it('derives archived, read-only rows that never read as unread', () => {
    const started = new Date(2026, 9, 19, 9, 5).getTime();
    const rows = plannerHistoryRows({ cardId: 'card', trackId: 'track', trackTitle: 'Track', provider: 'claude' },
      [{ ordinal: 0, started_at_ms: started, ended_at_ms: started + 1000 }]);
    expect(rows).toEqual([{
      id: 'card#0', trackId: 'track', trackTitle: 'Track', provider: 'claude',
      title: 'Earlier Planner conversation · 2026-10-19 09:05', kind: PLANNER_HISTORY_KIND, state: null,
      archivedAt: started + 1000, updatedAt: started + 1000, lastTurnCompletedAt: null,
    }]);
  });
});
