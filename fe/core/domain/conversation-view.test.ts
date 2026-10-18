import { expect, it } from 'vitest';
import { SYSTEM_PRESENTATION_LABELS, type ConversationActivity, type ConversationSystemEntry,
  type ConversationTurn, type ConversationTurnOutcome, type TranscriptEntry } from './conversation.js';
import { groupTranscriptActivities, keyTranscriptGroups, noTranscriptGroupKeys } from './conversation-groups.js';
import { foldQuietSyncs } from './conversation-quiet-sync.js';
import { projectTranscriptView, type TranscriptView } from './conversation-view.js';

function activity(id: string): ConversationActivity {
  return { id, author: 'activity', verb: 'Ran', target: id, tool: null,
    state: 'done', durationMs: null, detail: null, atMs: 2 };
}
function message(id: string, author: ConversationTurn['author'] = 'agent'): ConversationTurn {
  return { id, author, text: id, atMs: 3 };
}
function report(id: string): ConversationSystemEntry {
  return { id, author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited,
    text: 'Report edited (author = "user").', quiet: true, atMs: 1 };
}
const completed: ConversationTurnOutcome = { id: 'completed', author: 'turn', turnId: 'turn', elapsedMs: null, status: 'completed', atMs: 3 };
const next = (turns: readonly TranscriptEntry[], previous: TranscriptView | null = null) =>
  projectTranscriptView(turns, previous?.memory ?? noTranscriptGroupKeys(), previous);

it('reuses unchanged message, activity and quiet-sync bodies across a tail text edit', () => {
  const first = message('first', 'you');
  const calls = [activity('a'), activity('b')];
  const quiet = [report('report'), activity('quiet'), completed];
  const boundary = message('boundary', 'you');
  const tail = message('tail');
  const before = next([first, ...calls, ...quiet, boundary, tail]);
  const after = next([first, ...calls, ...quiet, boundary, { ...tail, text: 'growing' }], before);
  expect(after.groups).toHaveLength(before.groups.length);
  for (let index = 0; index < before.groups.length - 1; index++) {
    expect(after.groups[index].body).toBe(before.groups[index].body);
  }
  expect(after.groups[1].activities).toBe(before.groups[1].activities);
  expect(after.groups[2].body.quiet).toBe(before.groups[2].body.quiet);
  expect(after.groups.at(-1)?.body).not.toBe(before.groups.at(-1)?.body);
});

it('keeps body identity separate from positions when earlier groups are prepended', () => {
  const entries = [message('question', 'you'), activity('a'), activity('b'), message('tail')];
  const before = next(entries);
  const after = next([message('older', 'you'), ...entries], before);
  for (const [index, group] of before.groups.entries()) {
    expect(after.groups[index + 1].body).toBe(group.body);
    expect(after.groups[index + 1].index).toBe(group.index + 1);
  }
});

it('invalidates changed tool data and membership even when a run keeps its key', () => {
  const a = activity('a');
  const b = activity('b');
  const before = next([a, b]);
  const changed = next([a, { ...b, state: 'failed', detail: 'failure' }], before);
  expect(changed.groups[0].key).toBe(before.groups[0].key);
  expect(changed.groups[0].body).not.toBe(before.groups[0].body);
  expect(changed.groups[0].activities?.[1].state).toBe('failed');
  const appended = next([a, b, activity('c')], before);
  expect(appended.groups[0].key).toBe(before.groups[0].key);
  expect(appended.groups[0].body).not.toBe(before.groups[0].body);
  expect(appended.groups[0].activities).toHaveLength(3);
});

it('recomputes a quiet verdict when a lifted ask changes without joining its body', () => {
  const opening = report('report');
  const call = activity('call');
  const before = next([opening, call, completed]);
  const ask = { ...message('ask'), origin: 'ask' as const };
  const after = next([opening, call, completed, ask], before);
  expect(before.groups[0].body.quiet?.outcome).toBe('accepted');
  expect(after.groups[0].body.quiet?.entries).toEqual(before.groups[0].body.quiet?.entries);
  expect(after.groups[0].body.quiet?.outcome).toBeNull();
  expect(after.groups[0].body).not.toBe(before.groups[0].body);
  expect(after.groups[1].entry).toBe(ask);
  const authored = next([{ ...opening, text: 'Report edited (author = "plugin").' }, call, completed], before);
  expect(authored.groups[0].body.quiet?.author).toBe('plugin');
});

it('preserves canonical ordering, splits, merges and quiet grammar through wider changes', () => {
  const a = activity('a');
  const b = activity('b');
  const reportEntry = report('report');
  const quietCall = activity('quiet');
  const ask = { ...message('ask'), origin: 'ask' as const };
  const question = message('question', 'you');
  const speech = message('speech');
  const budget: ConversationSystemEntry = { id: 'budget', author: 'system', quiet: true,
    label: SYSTEM_PRESENTATION_LABELS.system_planner_turn_budget, text: 'Budget', atMs: 1 };
  const sequences: readonly (readonly TranscriptEntry[])[] = [
    [a, b], [a, speech, b], [b, a], [budget, reportEntry, quietCall, completed, ask, question, a, b],
    [reportEntry, quietCall, { ...completed, status: 'failed' }, question, b], [question, a], [], [a, b],
  ];
  let previous: TranscriptView | null = null;
  for (const turns of sequences) {
    const blocks = foldQuietSyncs(turns);
    const canonical = keyTranscriptGroups(groupTranscriptActivities(blocks.map(block => block.kind === 'entry' ? block.entry : block.entries[0])),
      previous?.memory ?? noTranscriptGroupKeys());
    const projected = next(turns, previous);
    expect(projected.groups).toHaveLength(canonical.groups.length);
    for (const [index, group] of canonical.groups.entries()) expect(projected.groups[index]).toMatchObject(group);
    expect(projected.groups.flatMap(group => group.body.quiet ?? [])).toEqual(blocks.filter(block => block.kind === 'quiet-sync'));
    expect(projected.memory).toEqual(canonical.memory);
    previous = projected;
  }
});

it('retains key history but no removed body cache beyond the current committed view', () => {
  const entries = [activity('a'), activity('b')];
  const before = next(entries);
  const empty = next([], before);
  const returned = next(entries, empty);
  expect(empty.groups).toEqual([]);
  expect(returned.groups[0].key).toBe(before.groups[0].key);
  expect(returned.groups[0].body).not.toBe(before.groups[0].body);
  expect(before.memory.byActivity.size).toBe(2);
  expect(empty.memory).toEqual(before.memory);
});
