import { describe, expect, it } from 'vitest';

import {
  groupTranscriptActivities, keyTranscriptGroups, noTranscriptGroupKeys, type TranscriptGroupKeys,
} from './conversation-groups.js';
import type { ConversationActivity, TranscriptEntry } from './conversation.js';

function activity(id: string, overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return {
    id, author: 'activity', verb: 'Ran', target: `tool-${id}`, state: 'done',
    tool: null, durationMs: null, detail: null, atMs: 1, ...overrides,
  };
}

const you: TranscriptEntry = { id: 'you', author: 'you', text: 'Do it', atMs: 2 };
const agent: TranscriptEntry = { id: 'agent', author: 'agent', text: 'Done', atMs: 3 };
const system: TranscriptEntry = { id: 'sys', author: 'system', label: 'Report edited', text: 'changed', atMs: 4 };

describe('groupTranscriptActivities', () => {
  it('returns nothing for an empty transcript', () => {
    expect(groupTranscriptActivities([])).toEqual([]);
  });

  it('leaves a lone activity as a group of one at its own index', () => {
    const turns = [you, activity('a')];
    expect(groupTranscriptActivities(turns)).toEqual([
      { index: 0, entry: you, activities: null },
      { index: 1, entry: turns[1], activities: [turns[1]] },
    ]);
  });

  it('joins adjacent activities under the first one, keeping the first index', () => {
    const turns = [you, activity('a'), activity('b'), activity('c')];
    const groups = groupTranscriptActivities(turns);
    expect(groups).toHaveLength(2);
    expect(groups[1]).toEqual({ index: 1, entry: turns[1], activities: [turns[1], turns[2], turns[3]] });
  });

  it.each([
    ['you', you],
    ['agent', agent],
    ['system', system],
  ])('never joins across a %s message', (_author, message) => {
    const turns = [activity('a'), activity('b'), message, activity('c'), activity('d')];
    const groups = groupTranscriptActivities(turns);
    expect(groups.map((group) => group.index)).toEqual([0, 2, 3]);
    expect(groups[0]?.activities?.map((entry) => entry.id)).toEqual(['a', 'b']);
    expect(groups[1]).toEqual({ index: 2, entry: message, activities: null });
    expect(groups[2]?.activities?.map((entry) => entry.id)).toEqual(['c', 'd']);
  });

  it('keeps the first entry and index stable as calls arrive and finish', () => {
    const before = [you, activity('a'), activity('b', { state: 'running', verb: 'Running' })];
    const after = [you, activity('a'), activity('b'), activity('c', { state: 'running', verb: 'Running' })];
    const [, first] = groupTranscriptActivities(before);
    const [, second] = groupTranscriptActivities(after);
    expect(first?.entry.id).toBe('a');
    expect(second?.entry.id).toBe('a');
    expect(first?.index).toBe(1);
    expect(second?.index).toBe(1);
    expect(first?.activities?.map((entry) => entry.state)).toEqual(['done', 'running']);
    expect(second?.activities?.map((entry) => entry.state)).toEqual(['done', 'done', 'running']);
  });

  it('does not mutate the transcript it reads', () => {
    const turns = Object.freeze([activity('a'), activity('b')]);
    expect(() => groupTranscriptActivities(turns)).not.toThrow();
    expect(turns).toHaveLength(2);
  });
});

/* A run keeps its key for as long as any call it had is still in it, or comes back. */
describe('keyTranscriptGroups', () => {
  /** Keys through a sequence of transcripts, each fed the memory of the last. */
  function keysThrough(transcripts: readonly (readonly TranscriptEntry[])[]) {
    let memory: TranscriptGroupKeys = noTranscriptGroupKeys();
    return transcripts.map((turns) => {
      const keyed = keyTranscriptGroups(groupTranscriptActivities(turns), memory);
      memory = keyed.memory;
      return { keys: keyed.groups.map((group) => group.key), memory };
    });
  }
  const runKey = (turns: readonly TranscriptEntry[]) => keysThrough([turns])[0].keys;

  it('keys a message by its own id and every run by a key of its own', () => {
    const keys = runKey([activity('a'), activity('b'), you, activity('c'), agent, activity('d'), activity('e')]);
    expect(keys[1]).toBe('you');
    expect(keys[3]).toBe('agent');
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys[2]).toMatch(/^group:/);
  });

  it('keeps the key when earlier calls of the same run are prepended, twice, and a new call is appended', () => {
    const [first, prepended, again, appended] = keysThrough([
      [activity('r1'), activity('r2')],
      [activity('e1'), activity('e2'), activity('r1'), activity('r2')],
      [activity('d1'), activity('e1'), activity('e2'), activity('r1'), activity('r2')],
      [activity('d1'), activity('e1'), activity('e2'), activity('r1'), activity('r2'), activity('r3', { state: 'running' })],
    ]);
    expect(prepended.keys).toEqual(first.keys);
    expect(again.keys).toEqual(first.keys);
    expect(appended.keys).toEqual(first.keys);
  });

  it('keeps the key when a call finishes in place', () => {
    const [before, after] = keysThrough([
      [you, activity('a'), activity('b', { state: 'running', verb: 'Running' })],
      [you, activity('a'), activity('b')],
    ]);
    expect(after.keys).toEqual(before.keys);
  });

  it('keeps the key when a refetch drops the head of the run', () => {
    const [before, after] = keysThrough([
      [activity('e1'), activity('e2'), activity('r1')],
      [activity('e2'), activity('r1')],
    ]);
    expect(after.keys).toEqual(before.keys);
  });

  it('gives a run prepended ahead of a known one a key of its own, and leaves the known one its key', () => {
    const [before, after] = keysThrough([
      [you, activity('r1'), activity('r2')],
      [activity('e1'), activity('e2'), agent, you, activity('r1'), activity('r2')],
    ]);
    expect(after.keys[3]).toBe(before.keys[1]);
    expect(after.keys[0]).not.toBe(before.keys[1]);
  });

  it('never reissues the key of a run that vanished whole', () => {
    const [before, after] = keysThrough([
      [activity('a'), activity('b')],
      [agent, activity('c'), activity('d')],
    ]);
    expect(after.keys[1]).not.toBe(before.keys[0]);
  });

  it('lets the first piece of a split run keep the key and issues the second a new one', () => {
    const [before, after] = keysThrough([
      [activity('a'), activity('b'), activity('c'), activity('d')],
      [activity('a'), activity('b'), agent, activity('c'), activity('d')],
    ]);
    expect(after.keys[0]).toBe(before.keys[0]);
    expect(after.keys[2]).not.toBe(before.keys[0]);
    expect(after.keys[2]).not.toBe(after.keys[0]);
  });

  it('keeps the key of a run that left the window whole when its calls come back', () => {
    const [before, gone, back] = keysThrough([
      [activity('a'), activity('b')],
      [agent],
      [activity('a'), activity('b'), agent],
    ]);
    expect(gone.keys).toEqual(['agent']);
    expect(back.keys[0]).toBe(before.keys[0]);
    expect(back.memory.byActivity.get('a')).toBe(before.keys[0]);
  });

  it('remembers a run through more than one transcript without it', () => {
    const [before, , , back] = keysThrough([
      [activity('a'), activity('b')],
      [agent],
      [agent, you],
      [activity('a'), agent, you],
    ]);
    expect(back.keys[0]).toBe(before.keys[0]);
  });

  it('keeps a returning run and a stranger that appeared meanwhile apart', () => {
    const [before, stranger, both] = keysThrough([
      [activity('a'), activity('b')],
      [agent, activity('c'), activity('d')],
      [activity('a'), activity('b'), agent, activity('c'), activity('d')],
    ]);
    expect(stranger.keys[1]).not.toBe(before.keys[0]);
    expect(both.keys[0]).toBe(before.keys[0]);
    expect(both.keys[2]).toBe(stranger.keys[1]);
  });

  it('remembers each call of a split run by the piece it ended up in', () => {
    const [, split, secondAlone, firstAlone] = keysThrough([
      [activity('a'), activity('b'), activity('c'), activity('d')],
      [activity('a'), activity('b'), agent, activity('c'), activity('d')],
      [activity('c'), activity('d')],
      [activity('a'), activity('b')],
    ]);
    expect(secondAlone.keys[0]).toBe(split.keys[2]);
    expect(firstAlone.keys[0]).toBe(split.keys[0]);
  });

  it('is the same answer twice for the same transcript, and remembers every call it has seen', () => {
    const turns = [activity('a'), activity('b'), you, activity('c')];
    const [first, second] = keysThrough([turns, turns]);
    expect(second.keys).toEqual(first.keys);
    expect(second.memory).toEqual(first.memory);
    expect([...first.memory.byActivity.keys()].sort()).toEqual(['a', 'b', 'c']);
    const [, shrunk] = keysThrough([turns, [activity('c')]]);
    expect([...shrunk.memory.byActivity.keys()].sort()).toEqual(['a', 'b', 'c']);
    expect(shrunk.memory.byActivity.get('a')).toBe(first.keys[0]);
  });

  it('does not hand the memory it was given back mutated', () => {
    const turns = [activity('a'), activity('b')];
    const given = noTranscriptGroupKeys();
    keyTranscriptGroups(groupTranscriptActivities(turns), given);
    expect(given.byActivity.size).toBe(0);
    expect(given.issued).toBe(0);
  });
});
