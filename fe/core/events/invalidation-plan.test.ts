import { describe, expect, it } from 'vitest';

import { wireEventSchema, type WireEvent } from '../api/schemas.js';
import { invalidationPlanFor } from './invalidation-plan.js';

/* Hand-maintained on purpose, never derived from the policies. `harness.item.added` is deliberately
 * out: it is the highest-frequency event and is emitted before `persist_snapshot` commits. */
const CONVERSATION_LIST_KINDS = [
  'card.added', 'card.updated',
  'worker_session.started', 'worker_session.status_changed', 'worker_session.superseded',
  'harness.phase.changed', 'harness.user_message.enqueued', 'harness.queue.changed',
] as const;

function event(value: unknown): WireEvent {
  return value as WireEvent;
}

describe('invalidation plan behavior', () => {
  it('refreshes evidence after file publication without parsing opaque task IDs', () => {
    const settled = wireEventSchema.parse({ ev: 'task.file_publication_settled', data: { task_id: 'opaque-attempt', operation_id: 'publication' } });
    expect(invalidationPlanFor(settled)).toEqual({
      invalidate: [['track-files'], ['track-report']], remove: [], writeThrough: [],
    });
  });
  it('refreshes evidence after candidate verification without parsing opaque task IDs', () => {
    const settled = wireEventSchema.parse({ ev: 'task.candidate_verification_settled', data: { task_id: 'opaque-attempt', operation_id: 'verification' } });
    expect(invalidationPlanFor(settled)).toEqual({
      invalidate: [['track-files'], ['track-report']], remove: [], writeThrough: [],
    });
  });
  it('refreshes the settled track\'s files and report after a git delivery settles', () => {
    const settled = wireEventSchema.parse({
      ev: 'task.git_delivery_settled',
      data: {
        task_id: 'opaque-attempt', idempotency_key: 'opaque-attempt', track_id: 'track-7', card_id: 'worker',
        delivery_id: 'delivery-1', ordinal: 1,
        result: { kind: 'failed', code: 'unresolved', reason: 'probe unknown', retry_allowed: true },
        wake_reason: 'failed',
      },
    });
    expect(invalidationPlanFor(settled)).toEqual({
      invalidate: [['track-files', 'track-7'], ['track-report', 'track-7']], remove: [], writeThrough: [],
    });
  });

  it('refreshes task evidence after execution settlement without guessing an opaque attempt ID', () => {
    const settled = wireEventSchema.parse({
      ev: 'task.execution_settled',
      data: { task_id: 'opaque-attempt', operation_id: 'operation-1' },
    });
    expect(invalidationPlanFor(settled)).toEqual({
      invalidate: [['track-files'], ['track-report']], remove: [], writeThrough: [],
    });
  });

  it('write-through updates only an existing area before invalidating the list', () => {
    const value = event({ ev: 'area.updated', data: { id: 'c1', name: 'new' } });
    expect(invalidationPlanFor(value)).toEqual({
      invalidate: [['areas']],
      remove: [],
      writeThrough: [{ key: ['areas'], mode: 'replace-existing-area', value: value.data }],
    });
  });

  it('invalidates track projections and pending diagnostics for track updates', () => {
    expect(invalidationPlanFor(event({ ev: 'track.updated', data: { id: 'w1', area_id: 'c1' } }))).toEqual({
      invalidate: [
        ['tracks', 'area', 'c1'], ['track', 'w1'], ['track-files', 'w1'], ['tracks-range'],
        ['track-report'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  it('keeps a lifecycle event to the base track projections on its own', () => {
    expect(invalidationPlanFor(event({
      ev: 'track.lifecycle_changed', data: { id: 'w1', area_id: 'c1' },
    }))).toEqual({
      invalidate: [
        ['tracks', 'area', 'c1'], ['track', 'w1'], ['track-files', 'w1'], ['tracks-range'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  it('removes deleted track detail after invalidating its remaining projections', () => {
    expect(invalidationPlanFor(event({ ev: 'track.deleted', data: { id: 'w1', area_id: 'c1' } }))).toEqual({
      invalidate: [
        ['tracks', 'area', 'c1'], ['overlays', 'track'], ['tracks-range'], ['track-report'],
      ],
      remove: [['track', 'w1'], ['track-report', 'w1']],
      writeThrough: [],
    });
  });

  it('refreshes pending diagnostics after a task plan mutation', () => {
    expect(invalidationPlanFor(event({
      ev: 'plan.updated',
      data: { track_id: 'w1', changed_keys: ['task-1'], agent_message: 'canceled' },
    }))).toEqual({
      invalidate: [['track-report', 'w1']],
      remove: [],
      writeThrough: [],
    });
  });

  it('does not duplicate the companion event that already invalidates a projection change', () => {
    expect(invalidationPlanFor(event({
      ev: 'plan.updated', data: { track_id: 'w1', changed_keys: ['task-1'] },
    })).invalidate).toEqual([]);
  });

  it('invalidates card mutations immediately without suppression or debounce state', () => {
    expect(invalidationPlanFor(event({ ev: 'card.added', data: { track_id: 'w1' } }))).toEqual({
      invalidate: [
        ['track', 'w1'], ['track-files', 'w1'], ['track-report'],
        ['track-conversations', 'w1'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  it.each(['card.added', 'card.deleted'] as const)(
    'broadly refreshes cross-track card references after %s',
    (ev) => {
      const keys = invalidationPlanFor(event({ ev, data: { track_id: 'target' } })).invalidate;
      expect(keys).toContainEqual(['track-report']);
    },
  );

  /* Keyed BY TRACK: the bare prefix would still satisfy a contains-key assertion while every
   * open track refetched its list on every runtime tick of every other track. */
  it.each([
    ['card.added', { track_id: 'track-1' }],
    ['card.updated', { track_id: 'track-1' }],
    ['harness.phase.changed', { card_id: 'card-1', track_id: 'track-1' }],
    ['harness.user_message.enqueued', { card_id: 'card-1', track_id: 'track-1' }],
    ['worker_session.started', { card_id: 'card-1' }],
    ['worker_session.status_changed', { card_id: 'card-1' }],
    ['worker_session.superseded', { card_id: 'card-1' }],
  ] as const)('keys the track conversation list by its own track for %s', (ev, data) => {
    const keys = invalidationPlanFor(event({ ev, data }), { findTrackOwningCard: () => 'track-1' })
      .invalidate.filter((key) => key[0] === 'track-conversations');
    expect(keys).toEqual([['track-conversations', 'track-1']]);
  });

  it('resolves runtime projections through card ownership', () => {
    expect(invalidationPlanFor(
      event({ ev: 'worker_session.started', data: { card_id: 'card-1' } }),
      { findTrackOwningCard: () => 'track-1' },
    )).toEqual({
      invalidate: [
        ['track', 'track-1'], ['overlays', 'card'], ['track-files', 'track-1'], ['track-report', 'track-1'],
        ['track-conversations', 'track-1'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  /* An unresolvable card falls back to the bare prefix rather than dropping the key, so an open list is never stale forever. */
  it('falls back to the track-conversations prefix when card ownership is unknown', () => {
    expect(invalidationPlanFor(
      event({ ev: 'worker_session.status_changed', data: { card_id: 'card-1' } }),
      { findTrackOwningCard: () => null },
    )).toEqual({
      invalidate: [
        ['overlays', 'card'], ['track-files'], ['track-report'],
        ['track-conversations'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  it('silently omits a card-overlay track detail when ownership is unknown', () => {
    expect(invalidationPlanFor(
      event({ ev: 'overlay.set', data: { entity_kind: 'card', entity_id: 'card-1' } }),
      { findTrackOwningCard: () => null },
    )).toEqual({
      invalidate: [['overlays', 'card']],
      remove: [],
      writeThrough: [],
    });
  });

  it('uses track_id, then card ownership, then the broad track-files prefix', () => {
    const context = { findTrackOwningCard: (cardId: string) => cardId === 'card-1' ? 'track-1' : null };
    const ev = 'codex.worker_requested';
    expect(invalidationPlanFor(event({ ev, data: { track_id: 'direct' } }), context).invalidate)
      .toEqual([['track-files', 'direct'], ['track-report', 'direct']]);
    expect(invalidationPlanFor(event({ ev, data: { card_id: 'card-1' } }), context).invalidate)
      .toEqual([['track-files', 'track-1'], ['track-report', 'track-1']]);
    expect(invalidationPlanFor(event({ ev, data: {} }), context).invalidate)
      .toEqual([['track-files'], ['track-report']]);
  });

  it.each(['codex.hook', 'claude.hook'] as const)('resolves a track for %s but stops at track-files', (ev) => {
    const context = { findTrackOwningCard: (cardId: string) => cardId === 'card-1' ? 'track-1' : null };
    expect(invalidationPlanFor(event({ ev, data: { track_id: 'direct' } }), context).invalidate)
      .toEqual([['track-files', 'direct']]);
    expect(invalidationPlanFor(event({ ev, data: { card_id: 'card-1' } }), context).invalidate)
      .toEqual([['track-files', 'track-1']]);
    expect(invalidationPlanFor(event({ ev, data: {} }), context).invalidate)
      .toEqual([['track-files']]);
  });

  it('invalidates terminal runtime projection through card ownership', () => {
    expect(invalidationPlanFor(
      event({ ev: 'terminal.deleted', data: { card_id: 'card-1' } }),
      { findTrackOwningCard: () => 'track-1' },
    ).invalidate).toEqual([['track-files', 'track-1'], ['track-report', 'track-1']]);
  });

  it('plans each harness event against the projections it can change', () => {
    const planned = (ev: WireEvent['ev']) => invalidationPlanFor(
      event({ ev, data: { card_id: 'card-1', track_id: 'track-1' } }),
    ).invalidate;
    expect(planned('harness.item.added')).toEqual([['harness-items', 'card-1']]);
    // The phase event delivers the turn outcome row, which emits no `harness.item.added` of its own.
    expect(planned('harness.phase.changed')).toEqual([
      ['planner-run', 'card-1'], ['harness-items', 'card-1'], ['track-conversations', 'track-1'],
      ['track', 'track-1'],
    ]);
    expect(planned('harness.transcript.cleared')).toEqual([
      ['harness-items', 'card-1'], ['planner-run', 'card-1'],
    ]);
    expect(planned('harness.user_message.enqueued')).toEqual([
      ['harness-items', 'card-1'], ['planner-run', 'card-1'],
      ['track-conversations', 'track-1'],
    ]);
    // `harness-items` is for `steered` (adds a transcript row) and `restored` (its sweep deletes it again).
    expect(planned('harness.queue.changed')).toEqual([
      ['planner-run', 'card-1'], ['harness-items', 'card-1'],
      ['track-conversations', 'track-1'],
    ]);
  });

  it('refetches the track conversation list from exactly the eight session-writing kinds', () => {
    const kinds = wireEventSchema.options.map((schema) => schema.shape.ev.value);
    const actual = kinds.filter((kind) => invalidationPlanFor({ ev: kind, data: {} } as WireEvent)
      .invalidate.some((key) => key[0] === 'track-conversations'));
    expect(new Set(actual)).toEqual(new Set(CONVERSATION_LIST_KINDS));
    expect(actual).toHaveLength(CONVERSATION_LIST_KINDS.length);
  });

  it('returns an empty plan for explicit no-op policies', () => {
    const empty = { invalidate: [], remove: [], writeThrough: [] };
    expect(invalidationPlanFor(event({ ev: 'plugin.state', data: {} }))).toEqual(empty);
    expect(invalidationPlanFor(event({ ev: 'proposal.resolved', data: {} }))).toEqual(empty);
  });

  it('silently ignores an unknown event kind received across versions', () => {
    expect(invalidationPlanFor(event({ ev: 'zzz.unknown', data: {} }))).toEqual({
      invalidate: [],
      remove: [],
      writeThrough: [],
    });
  });
});
