import { describe, expect, it } from 'vitest';
import type { HarnessItem } from '../api/generated/wire.js';
import { harnessInputSegmentSchema, harnessItemsOperation, harnessItemToTurns, type ConversationTurn } from './conversation.js';
import { matchSendOps, outboxView, type SendOp } from './conversation-outbox.js';

const RAW_SCOPE = Object.freeze({ cardId: 'card', threadId: 'thread' });

const accepted = (key: string, entryId: string, queueRevision: number | null): SendOp & { phase: 'confirmed' } => ({
  key, phase: 'confirmed', queueRevision, fromComposer: true, replaces: null,
  echo: { id: `echo-${key}`, author: 'you', text: 'same', atMs: 2,
    serverHighWaterBefore: 0, queued: true, entryId },
});

describe('canonical queue entry provenance', () => {
  it('preserves each decoded input identity even after a preceding harvested user segment', () => {
    const segments = ['harvested', 'first-input'].map(id => harnessInputSegmentSchema.parse({
      presentation: 'user', text: 'User says: same', attachments: [],
      origin: { observation: 'user', event_id: null, queue_entry: { id, rev: 0 } },
    }));
    const item: HarnessItem = { id: 7, worker_session_id: 'session', card_id: 'card', track_id: 'track',
      thread_id: 'thread', turn_id: null, item_uuid: 'harvested', item_type: 'userMessage',
      method: 'item/completed', params: '{}', created_at_ms: 1, input_segments: segments, turn_error_text: null };
    expect(harnessItemToTurns(item)).toMatchObject([
      { id: '7:0', queueEntryId: 'harvested' }, { id: '7:1', queueEntryId: 'first-input' },
    ]);
  });

  it('does not spend a different known entry merely because its words are equal', () => {
    const earlier = { id: '7:0', author: 'you', text: 'same', atMs: 1, queueEntryId: 'first-input' } as const;
    expect([...matchSendOps([earlier], [accepted('later-send', 'later-input', null)], [], 'card')]).toEqual([]);
  });

  it('lets folded accepted operations share their exact carrier even when it already retired another op', () => {
    const carrier: ConversationTurn & { queueEntryId: string } = {
      id: '7:1', author: 'you', text: 'combined words', atMs: 1, queueEntryId: 'surviving-entry',
      inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 1, turnId: 'real-folded-turn', entryRev: 1 },
    };
    expect([...matchSendOps([carrier], [accepted('create', 'surviving-entry', 0), accepted('folded', 'surviving-entry', 1)], ['7:1'], 'card')])
      .toEqual([['create', '7:1'], ['folded', '7:1']]);
  });

  it('does not let an already-seen same-origin projection consume a later ordinary folded submission', () => {
    const prior = { id: '7:1', author: 'you', text: 'same', atMs: 1, queueEntryId: 'surviving-entry',
      inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 1, turnId: null, entryRev: 1 } } as const;
    const folded = accepted('later-fold', 'surviving-entry', 1);
    const op: SendOp = { ...folded, echo: { ...folded.echo, serverHighWaterBefore: 7 } };
    expect([...matchSendOps([prior], [op], [], 'card')]).toEqual([]);
    expect([...matchSendOps([prior, { ...prior, id: '9:1', text: 'same\n\nsame', inputSource: { ...prior.inputSource, itemId: 9, entryRev: 2, turnId: 'folded-provider-turn' } }], [op], ['9:1'], 'card')]).toEqual([['later-fold', '9:1']]);
  });
});


it('does not let an older unbound source retire an ordinary queued edit whose real revision is newer', () => {
  const original = accepted('ordinary', 'known', null);
  const op: SendOp = { ...original, queueRevision: 2, echo: { ...original.echo, text: 'Actual revision two' } };
  const stale: ConversationTurn = { id: '7:0', author: 'you', text: 'Older revision zero', atMs: 1, queueEntryId: 'known',
    inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: null, entryRev: 0 } };
  const shown = outboxView({ cardId: 'card', serverEntries: [stale], serverTurns: [stale], queuedEntryIds: new Set(), stalled: false,
    ops: [op], spent: [], landed: { transcript: 1, run: 1 }, liveReplies: [] });
  expect(shown.retire).toEqual([]);
  expect(shown.transcript.map(entry => 'text' in entry ? entry.text : entry.author)).toEqual(['Actual revision two']);
});

it('rejects the unreleased bare identity and preserves explicit unknown legacy source version', () => {
  const segment = { presentation: 'user', text: 'Legacy text', attachments: [], origin: { observation: 'user', event_id: null, queue_entry: null } };
  expect(harnessInputSegmentSchema.safeParse(segment).success).toBe(true);
  expect(harnessInputSegmentSchema.safeParse({ ...segment, origin: { observation: 'user', event_id: null, queue_entry_id: 'old-unreleased-id' } }).success).toBe(false);
  expect(harnessInputSegmentSchema.safeParse({ ...segment, origin: { ...segment.origin, queue_entry: { id: 'known' } } }).success).toBe(false);
});


it('decodes released legacy origin through the real history page while rejecting present malformed queue references', () => {
  const operation = harnessItemsOperation('card');
  const row = { id: 7, worker_session_id: 'session', card_id: 'card', track_id: 'track', thread_id: 'thread', turn_id: null,
    item_uuid: 'legacy-user', item_type: 'userMessage', method: 'item/completed', params: '{}', created_at_ms: 1, turn_error_text: null };
  const segment = { presentation: 'user', text: 'User says:\nReleased words', attachments: [], origin: { observation: 'user_message', event_id: null } };
  const legacy = operation.responseSchema.parse([{ ...row, input_segments: [segment] }]);
  expect(legacy[0].input_segments?.[0].origin).toEqual({ ...segment.origin, queue_entry: null });
  expect(harnessItemToTurns(legacy[0])).toMatchObject([{ text: 'Released words', queueEntryId: null, inputSource: { entryRev: null, turnId: null } }]);
  for (const ref of [null, { id: 'known-entry', rev: 2 }]) {
    const parsed = operation.responseSchema.parse([{ ...row, input_segments: [{ ...segment, origin: { ...segment.origin, queue_entry: ref } }] }]);
    expect(parsed[0].input_segments?.[0].origin?.queue_entry).toEqual(ref);
  }
  for (const ref of [undefined, {}, { id: 'known-entry' }, { rev: 2 }, { id: 'known-entry', rev: -1 }, { id: '', rev: 0 }]) {
    expect(operation.responseSchema.safeParse([{ ...row, input_segments: [{ ...segment, origin: { ...segment.origin, queue_entry: ref } }] }]).success).toBe(false);
  }
});


it('keeps an ordinary send with unknown revision until bounded body matching or an eligible known source version', () => {
  const op = { ...accepted('fresh-fold', 'surviving-entry', null), echo: { ...accepted('fresh-fold', 'surviving-entry', null).echo,
    text: 'Newly accepted body B', serverHighWaterBefore: 3 } };
  const older: ConversationTurn = { id: '7:0', author: 'you', text: 'Older retained body A', atMs: 1, queueEntryId: 'surviving-entry',
    inputSource: { ...RAW_SCOPE, itemId: 7, segmentIndex: 0, turnId: null, entryRev: 0 } };
  const shown = outboxView({ cardId: 'card', serverEntries: [older], serverTurns: [older], queuedEntryIds: new Set(), stalled: false,
    ops: [op], spent: [], landed: { transcript: 1, run: 1 }, liveReplies: [] });
  expect(shown.retire).toEqual([]);
  expect(shown.transcript.some(entry => 'text' in entry && entry.text === op.echo.text)).toBe(true);
  const current = { ...older, id: '8:0', text: op.echo.text, inputSource: { ...RAW_SCOPE, itemId: 8, segmentIndex: 0, turnId: 'real-turn', entryRev: 1 } };
  expect([...matchSendOps([current], [op], [], 'card')]).toEqual([[op.key, current.id]]);
  expect([...matchSendOps([{ ...current, queueEntryId: 'different-entry' }], [op], [], 'card')]).toEqual([]);
  const known = { ...op, queueRevision: 1 };
  expect([...matchSendOps([{ ...older, text: op.echo.text }], [known], [], 'card')]).toEqual([]);
  expect([...matchSendOps([{ ...current, inputSource: { ...current.inputSource, entryRev: null } }], [known], [], 'card')]).toEqual([]);
  expect([...matchSendOps([{ ...current, text: `Older retained body A\n\n${op.echo.text}` }], [known], [], 'card')]).toEqual([[op.key, current.id]]);
});
