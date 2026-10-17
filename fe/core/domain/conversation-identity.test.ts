import { describe, expect, it } from 'vitest';
import type { HarnessItem } from '../api/generated/wire.js';
import { harnessInputSegmentSchema, harnessItemToTurns, type ConversationTurn } from './conversation.js';
import { matchSendOps, type SendOp } from './conversation-outbox.js';

const accepted = (key: string, entryId: string): SendOp => ({
  key, phase: 'confirmed', fromComposer: true, replaces: null,
  echo: { id: `echo-${key}`, author: 'you', text: 'same', atMs: 2,
    serverHighWaterBefore: 0, queued: true, entryId },
});

describe('canonical queue entry provenance', () => {
  it('preserves each decoded input identity even after a preceding harvested user segment', () => {
    const segments = ['harvested', 'first-input'].map(queue_entry_id => harnessInputSegmentSchema.parse({
      presentation: 'user', text: 'User says: same', attachments: [],
      origin: { observation: 'user', event_id: null, queue_entry_id },
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
