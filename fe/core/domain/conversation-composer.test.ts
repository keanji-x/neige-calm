import { expect, it } from 'vitest';
import type { HarnessInputSegment } from '../api/generated/wire.js';
import { buildTranscript, MAX_ATTACHMENTS_PER_MESSAGE, type TranscriptEntry } from './conversation.js';
import {
  editedTurnMessageIds, editedTurnRefill, isLatestTurn, isSameComposer,
  withoutEditedTurn, withRefill,
} from './conversation-composer.js';

const image = (id: string) => ({ id, contentType: 'image/png', size: 4, url: `/api/cards/c/planner/attachments/${id}` });

/** One stored row of the thread, as `GET …/harness/items` lists it. */
type StoredRow = Parameters<typeof buildTranscript>[0][number];
function row(id: number, turnId: string, itemType: string | null, method: string, params: unknown,
  segments?: readonly HarnessInputSegment[]): StoredRow {
  return {
    id, worker_session_id: 'r', card_id: 'c', track_id: 't', thread_id: 'th', turn_id: turnId, turn_error_text: null,
    item_uuid: null, item_type: itemType, method, params: JSON.stringify(params), created_at_ms: id,
    ...(segments === undefined ? {} : { input_segments: [...segments] }),
  };
}
const said = (id: number, turnId: string, segments: readonly HarnessInputSegment[]) =>
  row(id, turnId, 'userMessage', 'item/completed', { item: { content: [] }, completedAtMs: id }, segments);
const answer = (id: number, turnId: string, text: string) =>
  row(id, turnId, 'agentMessage', 'item/completed', { item: { text }, completedAtMs: id });
const ended = (id: number, turnId: string) =>
  row(id, turnId, null, 'turn/completed', { id: turnId, status: 'completed', error: null });

it('refills from the transcript what the turn said: prompt and steers, lead dropped, each image once', () => {
  const prompt = { presentation: 'user' as const, text: 'User says:\n  Original prompt\n', attachments: [image('a.png')] };
  const steer = { presentation: 'user' as const, text: 'User says:\nA steer', attachments: [image('a.png'), image('b.png')] };
  const picture = { presentation: 'user' as const, text: 'User says:\n', attachments: [image('c.png')] };
  const entries = buildTranscript([
    said(1, 'turn-0', [{ presentation: 'user', text: 'User says:\nEarlier', attachments: [image('z.png')] }]),
    answer(2, 'turn-0', 'Earlier answer'), ended(3, 'turn-0'),
    said(11, 'turn-1', [prompt]), said(12, 'turn-1', [steer]), said(13, 'turn-1', [picture]),
    answer(14, 'turn-1', 'Answer'), ended(15, 'turn-1'),
  ]);
  expect(editedTurnRefill(entries, 'outcome-15')).toEqual({
    text: 'Original prompt\n\nA steer', attachments: [image('a.png'), image('b.png'), image('c.png')],
  });
  expect(withoutEditedTurn(entries, 'outcome-15')).toEqual(entries.slice(0, 3));
});

const you = (id: string, text: string): TranscriptEntry => ({ id, author: 'you', text, atMs: 1 });
const agent = (id: string, text: string, origin?: 'ask'): TranscriptEntry =>
  ({ id, author: 'agent', text, atMs: 1, ...(origin === undefined ? {} : { origin }) });
const outcome = (id: string, turnId: string): TranscriptEntry =>
  ({ id, author: 'turn', turnId, status: 'completed', elapsedMs: null, atMs: 1 });

it('hides the turn from its first message of yours, never what came between the turns', () => {
  const entries = [
    you('u0', 'Earlier'), agent('a0', 'Earlier answer'), outcome('o0', 'turn-0'),
    agent('n', 'A question between turns', 'ask'),
    you('u1', 'Prompt'), agent('a1', 'Answer'), outcome('o1', 'turn-1'),
  ];
  expect(withoutEditedTurn(entries, 'o1').map((entry) => entry.id)).toEqual(['u0', 'a0', 'o0', 'n']);
  expect(editedTurnRefill(entries, 'o1')).toEqual({ text: 'Prompt', attachments: [] });
  /* The first turn has no outcome before it. */
  expect(withoutEditedTurn(entries, 'o0').map((entry) => entry.id)).toEqual(['n', 'u1', 'a1', 'o1']);
});

it('marks only that turn’s messages of yours, and knows it for the latest only while nothing follows its outcome', () => {
  const entries = [
    you('u0', 'Earlier'), outcome('o0', 'turn-0'),
    you('u1', 'Prompt'), you('u2', 'Steer'), agent('a1', 'Answer'), outcome('o1', 'turn-1'),
  ];
  expect([...editedTurnMessageIds(entries, 'o1')]).toEqual(['u1', 'u2']);
  expect([...editedTurnMessageIds(entries, 'gone')]).toEqual([]);
  expect(isLatestTurn(entries, 'o1')).toBe(true);
  expect(isLatestTurn(entries, 'o0')).toBe(false);
  expect(isLatestTurn([...entries, you('u3', 'From elsewhere')], 'o1')).toBe(false);
  expect(isLatestTurn(entries.slice(0, 2), 'o1')).toBe(false);
});

it('finds nothing to edit or hide for an outcome the transcript no longer shows, or a turn with no message of yours', () => {
  const entries = [you('u0', 'Earlier'), outcome('o0', 'turn-0'), agent('a1', 'Automatic'), outcome('o1', 'turn-1')];
  expect(editedTurnRefill(entries, 'gone')).toBeNull();
  expect(withoutEditedTurn(entries, 'gone')).toBe(entries);
  expect(editedTurnRefill(entries, 'o1')).toBeNull();
  expect(withoutEditedTurn(entries, 'o1')).toBe(entries);
});

it('tells an untouched refill from a changed one by its words and image ids in order', () => {
  const refill = { text: 'Prompt', attachments: [image('a.png'), image('b.png')] };
  expect(isSameComposer({ text: 'Prompt', attachments: [image('a.png'), image('b.png')] }, refill)).toBe(true);
  expect(isSameComposer({ text: 'Prompt ', attachments: refill.attachments }, refill)).toBe(false);
  expect(isSameComposer({ text: 'Prompt', attachments: [image('a.png')] }, refill)).toBe(false);
  expect(isSameComposer({ text: 'Prompt', attachments: [image('b.png'), image('a.png')] }, refill)).toBe(false);
});

it('adds a refill to a composer that already holds something, discarding nothing', () => {
  const refill = { text: 'Removed prompt', attachments: [image('a.png'), image('b.png')] };
  expect(withRefill({ text: '  ', attachments: [] }, refill)).toEqual(refill);
  expect(withRefill({ text: 'Typed', attachments: [image('a.png')] }, refill)).toEqual({
    text: 'Typed\n\nRemoved prompt', attachments: [image('a.png'), image('b.png')],
  });
  expect(withRefill({ text: 'Typed', attachments: [] }, { text: '', attachments: [image('c.png')] }))
    .toEqual({ text: 'Typed', attachments: [image('c.png')] });
});

/* #2068 item 19: the composer's one image cap holds for a refill as for a pick, so a refill never makes a send the
   server must refuse. */
it('never takes more images than a message carries, keeping the composer’s own first', () => {
  const images = Array.from({ length: MAX_ATTACHMENTS_PER_MESSAGE + 1 }, (_, index) => image(`${index}.png`));
  expect(withRefill({ text: '', attachments: [] }, { text: 'Prompt', attachments: images }).attachments)
    .toEqual(images.slice(0, MAX_ATTACHMENTS_PER_MESSAGE));
  expect(withRefill({ text: '', attachments: [image('own.png')] }, { text: '', attachments: images }).attachments)
    .toEqual([image('own.png'), ...images.slice(0, MAX_ATTACHMENTS_PER_MESSAGE - 1)]);
});
