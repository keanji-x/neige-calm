import { describe, expect, it } from 'vitest';

import { currentResponseMessage } from './conversation-actions.js';
import { buildTranscript, type TranscriptEntry } from './conversation.js';

/* A turn's outcome reads after the last entry of its own turn (#1923 S2, design decision 6). */

type Row = Parameters<typeof buildTranscript>[0][number];

function row(
  id: number, turnId: string | null, method: string, itemType: string | null, item: unknown, uuid: string | null = `u${id}`,
): Row {
  return {
    id, worker_session_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: turnId,
    turn_error_text: null, item_uuid: uuid, item_type: itemType, method,
    params: JSON.stringify({ completedAtMs: 1000 + id, item }), created_at_ms: 1000 + id,
  };
}

function outcome(id: number, turnId: string, status = 'completed'): Row {
  return {
    id, worker_session_id: 'r', card_id: 'c', track_id: 'w', thread_id: 't', turn_id: turnId,
    turn_error_text: null, item_uuid: null, item_type: null, method: 'turn/completed',
    params: JSON.stringify({ id: turnId, status }), created_at_ms: 1000 + id,
  };
}

const said = (id: number, turnId: string | null, text: string) =>
  row(id, turnId, 'item/completed', 'userMessage', { content: [{ text }] });
const replyStarted = (id: number, turnId: string, uuid: string) =>
  row(id, turnId, 'item/started', 'agentMessage', { id: uuid, type: 'agentMessage', text: '' }, uuid);
const replied = (id: number, turnId: string, uuid: string, text: string) =>
  row(id, turnId, 'item/completed', 'agentMessage', { id: uuid, type: 'agentMessage', text }, uuid);

const line = (entry: TranscriptEntry): string =>
  entry.author === 'activity' ? entry.verb : entry.author === 'turn' ? `[${entry.status}]` : entry.text;

describe('buildTranscript outcome placement', () => {
  it('puts a Claude outcome stored between a reply\'s started and completed rows below the reply', () => {
    /* The production shape: rows 22941 started, 22942 outcome, 22943 completed. */
    const entries = buildTranscript([
      said(22940, 'T', 'question'),
      replyStarted(22941, 'T', 'msg:0'),
      outcome(22942, 'T', 'interrupted'),
      replied(22943, 'T', 'msg:0', 'the answer'),
    ]);
    expect(entries.map(line)).toEqual(['question', 'the answer', '[interrupted]']);
    /* The chat actions read the outcome as the end of the turn only while it is the last entry. */
    expect(currentResponseMessage(entries, true)?.text).toBe('the answer');
  });

  it('keeps a Codex partial, stored below its outcome, above the Interrupted line', () => {
    const entries = buildTranscript([
      said(1, 'T', 'question'),
      replyStarted(2, 'T', 'm'),
      row(3, 'T', 'item/completed', 'agentMessage', { id: 'm', type: 'agentMessage', text: 'half an' }, 'm'),
      outcome(4, 'T', 'interrupted'),
    ]);
    expect(entries.map(line)).toEqual(['question', 'half an', '[interrupted]']);
  });

  it('never moves an outcome past the first entry of a later turn', () => {
    const entries = buildTranscript([
      said(1, 'T1', 'first'),
      outcome(2, 'T1'),
      said(3, 'T2', 'second'),
      replied(4, 'T1', 'late', 'too late'),
    ]);
    expect(entries.map(line)).toEqual(['first', '[completed]', 'second', 'too late']);
  });

  it('passes over rows that belong to no turn to reach its own turn\'s last entry', () => {
    const entries = buildTranscript([
      said(1, 'T', 'question'),
      outcome(2, 'T'),
      said(3, null, 'no turn'),
      replied(4, 'T', 'm', 'answer'),
    ]);
    expect(entries.map(line)).toEqual(['question', 'no turn', 'answer', '[completed]']);
  });

  it('places an outcome by its entry position after pairing, not by its completed row', () => {
    /* A command that started before the outcome and completed after it keeps its started place. */
    const entries = buildTranscript([
      said(1, 'T', 'question'),
      row(2, 'T', 'item/started', 'commandExecution', { command: 'sleep 9' }, 'cmd'),
      outcome(3, 'T'),
      row(4, 'T', 'item/completed', 'commandExecution', { command: 'sleep 9', exitCode: 0 }, 'cmd'),
    ]);
    expect(entries.map(line)).toEqual(['question', 'Ran', '[completed]']);
  });

  it('leaves an outcome that already follows its turn where it is', () => {
    const entries = buildTranscript([
      said(1, 'T1', 'first'),
      replied(2, 'T1', 'a', 'one'),
      outcome(3, 'T1'),
      said(4, 'T2', 'second'),
      replied(5, 'T2', 'b', 'two'),
      outcome(6, 'T2'),
    ]);
    expect(entries.map(line)).toEqual(['first', 'one', '[completed]', 'second', 'two', '[completed]']);
  });
});
