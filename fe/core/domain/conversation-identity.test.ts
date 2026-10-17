import { describe, expect, it } from 'vitest';
import type { HarnessItem } from '../api/generated/wire.js';
import { harnessInputSegmentSchema, harnessItemToTurns, type ConversationTurn } from './conversation.js';
import { matchSendOps, outboxView, type SendOp } from './conversation-outbox.js';

const accepted = (key: string, entryId: string): SendOp & { phase: 'confirmed' } => ({
  key, phase: 'confirmed', queueRevision: null, fromComposer: true, replaces: null,
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
    expect([...matchSendOps([earlier], [accepted('later-send', 'later-input')], [])]).toEqual([]);
  });

  it('lets folded accepted operations share their exact carrier even when it already retired another op', () => {
    const carrier: ConversationTurn & { queueEntryId: string } = {
      id: '7:1', author: 'you', text: 'combined words', atMs: 1, queueEntryId: 'surviving-entry',
    };
    expect([...matchSendOps([carrier], [accepted('create', 'surviving-entry'), accepted('folded', 'surviving-entry')], ['7:1'])])
      .toEqual([['create', '7:1'], ['folded', '7:1']]);
  });

  it('does not let an already-seen same-origin projection consume a later ordinary folded submission', () => {
    const prior = { id: '7:1', author: 'you', text: 'same', atMs: 1, queueEntryId: 'surviving-entry' } as const;
    const folded = accepted('later-fold', 'surviving-entry');
    const op: SendOp = { ...folded, echo: { ...folded.echo, serverHighWaterBefore: 7 } };
    expect([...matchSendOps([prior], [op], [])]).toEqual([]);
    expect([...matchSendOps([prior, { ...prior, id: '9:1' }], [op], ['9:1'])]).toEqual([['later-fold', '9:1']]);
  });
});


it('does not let an older unbound source retire an ordinary queued edit whose real revision is newer', () => {
  const original = accepted('ordinary', 'known');
  const op: SendOp = { ...original, queueRevision: 2, echo: { ...original.echo, text: 'Actual revision two' } };
  const stale: ConversationTurn = { id: '7:0', author: 'you', text: 'Older revision zero', atMs: 1, queueEntryId: 'known',
    inputSource: { itemId: 7, segmentIndex: 0, turnId: null, entryRev: 0 } };
  const shown = outboxView({ serverEntries: [stale], serverTurns: [stale], queuedEntryIds: new Set(), stalled: false,
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
