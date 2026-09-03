import { describe, expect, it } from 'vitest';

import { wireEventSchema, type WireEvent } from '../api/schemas.js';
import { invalidationPlanFor } from './invalidation-plan.js';

/*
 * The expected side of the conversation-list test, written out here on purpose.
 *
 * It cannot be derived from the policies without becoming the thing it checks:
 * the production fact *is* "which policies push this key", so any expectation
 * read out of them would agree with itself no matter what changed. So this is
 * an independent list, maintained by hand, and its whole value is that adding
 * an eighth policy — or dropping one of these seven — has to be typed here as
 * well, deliberately, before the suite goes green again.
 *
 * The three `runtime.*` kinds joined the original four in #1189 §5.5: a row's
 * `state` comes from `worker_sessions.state`, and those are the events that
 * write it. `wave.lifecycle_changed` is the near miss that must stay out — it
 * changes a wave, not a session row, and the sessions it ends announce
 * themselves as `runtime.superseded`, which is already in this list.
 *
 * `harness.item.added` is a KNOWN and DELIBERATE gap, not an omission. It does
 * change what these lists show: the event's producer records the item and then
 * calls `persist_snapshot` (`harness/run_loop.rs`), which bumps
 * `worker_sessions.updated_at_ms` — and both lists are ordered by that column
 * (`routes/wave_conversations.rs`, mirrored by `conversation.ts`'s sort on
 * `updatedAt`). So two concurrent sessions where the older one keeps producing
 * items without a phase change will have server-side order that the cache does
 * not, until the next phase or runtime event.
 *
 * It is still out for two reasons, and adding it here would make things worse
 * rather than better. It is the highest-frequency event the kernel emits, so it
 * would refetch a wholesale list per item. And it *races the write it would be
 * reporting*: the event is emitted before `persist_snapshot` commits, so the
 * refetch it triggers can land on the pre-snapshot ordering and cache exactly
 * the stale order it was meant to fix. The real fix is a mergeable/throttled
 * activity signal emitted after the snapshot persists, or taking list recency
 * off these snapshot writes altogether — both new mechanisms, tracked in #1216,
 * out of scope for #1189 S4.
 */
const CONVERSATION_LIST_KINDS = [
  'card.added', 'card.updated',
  'runtime.started', 'runtime.status_changed', 'runtime.superseded',
  'harness.phase.changed', 'harness.user_message.enqueued',
] as const;

function event(value: unknown): WireEvent {
  return value as WireEvent;
}

describe('invalidation plan behavior', () => {
  it('write-through updates only an existing area before invalidating the list', () => {
    const value = event({ ev: 'area.updated', data: { id: 'c1', name: 'new' } });
    expect(invalidationPlanFor(value)).toEqual({
      invalidate: [['areas']],
      remove: [],
      writeThrough: [{ key: ['areas'], mode: 'replace-existing-area', value: value.data }],
    });
  });

  it('invalidates all four wave projections for wave updates', () => {
    expect(invalidationPlanFor(event({ ev: 'wave.updated', data: { id: 'w1', area_id: 'c1' } }))).toEqual({
      invalidate: [['waves', 'area', 'c1'], ['wave', 'w1'], ['wave-files', 'w1'], ['waves-range']],
      remove: [],
      writeThrough: [],
    });
  });

  it('removes deleted wave detail after invalidating its remaining projections', () => {
    expect(invalidationPlanFor(event({ ev: 'wave.deleted', data: { id: 'w1', area_id: 'c1' } }))).toEqual({
      invalidate: [['waves', 'area', 'c1'], ['overlays', 'wave'], ['waves-range']],
      remove: [['wave', 'w1']],
      writeThrough: [],
    });
  });

  it('invalidates card mutations immediately without suppression or debounce state', () => {
    expect(invalidationPlanFor(event({ ev: 'card.added', data: { wave_id: 'w1' } }))).toEqual({
      invalidate: [
        ['wave', 'w1'], ['wave-files', 'w1'], ['area-conversations'], ['wave-conversations', 'w1'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  /*
   * The wave list is keyed BY WAVE, and that is the assertion — not "a
   * wave-conversations key is present somewhere".
   *
   * `GET /api/waves/{wave_id}/conversations` is per-wave (#1189 §4.1), so the
   * query it backs is `['wave-conversations', waveId]`. Dropping the id here to
   * copy the area list's bare prefix would still invalidate the right query, by
   * prefix match, and every "contains the key" assertion would stay green while
   * every open wave refetched its list on every runtime tick of every other
   * wave. The area list is a prefix because its id is genuinely unrecoverable;
   * this one's is right there in the event.
   */
  it.each([
    ['card.added', { wave_id: 'wave-1' }],
    ['card.updated', { wave_id: 'wave-1' }],
    ['harness.phase.changed', { card_id: 'card-1', wave_id: 'wave-1' }],
    ['harness.user_message.enqueued', { card_id: 'card-1', wave_id: 'wave-1' }],
    ['runtime.started', { card_id: 'card-1' }],
    ['runtime.status_changed', { card_id: 'card-1' }],
    ['runtime.superseded', { card_id: 'card-1' }],
  ] as const)('keys the wave conversation list by its own wave for %s', (ev, data) => {
    const keys = invalidationPlanFor(event({ ev, data }), { findWaveOwningCard: () => 'wave-1' })
      .invalidate.filter((key) => key[0] === 'wave-conversations');
    expect(keys).toEqual([['wave-conversations', 'wave-1']]);
  });

  it('resolves runtime projections through card ownership', () => {
    expect(invalidationPlanFor(
      event({ ev: 'runtime.started', data: { card_id: 'card-1' } }),
      { findWaveOwningCard: () => 'wave-1' },
    )).toEqual({
      invalidate: [
        ['wave', 'wave-1'], ['overlays', 'card'], ['wave-files', 'wave-1'], ['wave-report', 'wave-1'],
        ['area-conversations'], ['wave-conversations', 'wave-1'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  /*
   * An unresolvable card falls back to the bare prefix rather than dropping the
   * key: "some wave's list may have changed" is true and cheap (an invalidated
   * key with no active observer only marks entries stale), whereas dropping it
   * would leave a genuinely open list stale forever.
   */
  it('falls back to the wave-conversations prefix when card ownership is unknown', () => {
    expect(invalidationPlanFor(
      event({ ev: 'runtime.status_changed', data: { card_id: 'card-1' } }),
      { findWaveOwningCard: () => null },
    )).toEqual({
      invalidate: [
        ['overlays', 'card'], ['wave-files'], ['wave-report'],
        ['area-conversations'], ['wave-conversations'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  it('silently omits a card-overlay wave detail when ownership is unknown', () => {
    expect(invalidationPlanFor(
      event({ ev: 'overlay.set', data: { entity_kind: 'card', entity_id: 'card-1' } }),
      { findWaveOwningCard: () => null },
    )).toEqual({
      invalidate: [['overlays', 'card']],
      remove: [],
      writeThrough: [],
    });
  });

  it('uses wave_id, then card ownership, then the broad wave-files prefix', () => {
    const context = { findWaveOwningCard: (cardId: string) => cardId === 'card-1' ? 'wave-1' : null };
    const ev = 'codex.worker_requested';
    expect(invalidationPlanFor(event({ ev, data: { wave_id: 'direct' } }), context).invalidate)
      .toEqual([['wave-files', 'direct'], ['wave-report', 'direct']]);
    expect(invalidationPlanFor(event({ ev, data: { card_id: 'card-1' } }), context).invalidate)
      .toEqual([['wave-files', 'wave-1'], ['wave-report', 'wave-1']]);
    expect(invalidationPlanFor(event({ ev, data: {} }), context).invalidate)
      .toEqual([['wave-files'], ['wave-report']]);
  });

  /*
   * A hook resolves the same wave the same way — it just stops at the
   * workspace. It fires roughly twice per tool call per running worker and
   * writes no `tasks` row, so paying a whole-document report projection for it
   * bought a value that could not have changed. The ladder is asserted again
   * here so "no report key" cannot be confused with "no wave resolution".
   */
  it.each(['codex.hook', 'claude.hook'] as const)('resolves a wave for %s but stops at wave-files', (ev) => {
    const context = { findWaveOwningCard: (cardId: string) => cardId === 'card-1' ? 'wave-1' : null };
    expect(invalidationPlanFor(event({ ev, data: { wave_id: 'direct' } }), context).invalidate)
      .toEqual([['wave-files', 'direct']]);
    expect(invalidationPlanFor(event({ ev, data: { card_id: 'card-1' } }), context).invalidate)
      .toEqual([['wave-files', 'wave-1']]);
    expect(invalidationPlanFor(event({ ev, data: {} }), context).invalidate)
      .toEqual([['wave-files']]);
  });

  it('invalidates terminal runtime projection through card ownership', () => {
    expect(invalidationPlanFor(
      event({ ev: 'terminal.deleted', data: { card_id: 'card-1' } }),
      { findWaveOwningCard: () => 'wave-1' },
    ).invalidate).toEqual([['wave-files', 'wave-1'], ['wave-report', 'wave-1']]);
  });

  it('plans each harness event against the projections it can change', () => {
    const planned = (ev: WireEvent['ev']) => invalidationPlanFor(
      event({ ev, data: { card_id: 'card-1', wave_id: 'wave-1' } }),
    ).invalidate;
    expect(planned('harness.item.added')).toEqual([['harness-items', 'card-1']]);
    expect(planned('harness.phase.changed')).toEqual([
      ['spec-run', 'card-1'], ['area-conversations'], ['wave-conversations', 'wave-1'],
    ]);
    expect(planned('harness.transcript.cleared')).toEqual([
      ['harness-items', 'card-1'], ['spec-run', 'card-1'],
    ]);
    expect(planned('harness.user_message.enqueued')).toEqual([
      ['harness-items', 'card-1'], ['spec-run', 'card-1'],
      ['area-conversations'], ['wave-conversations', 'wave-1'],
    ]);
  });

  /*
   * The list is refetched wholesale, so every extra trigger is a whole refetch
   * nobody asked for — and two triggers for one change make it impossible to
   * prove either one is doing the work. The `actual` side is read out of the
   * production planner by running every wire event kind through it; the
   * `expected` side is the hand-kept list above, and the point is that the two
   * are maintained separately.
   */
  it.each(['area-conversations', 'wave-conversations'] as const)(
    'refetches the %s list from exactly the seven session-writing kinds',
    (root) => {
      const kinds = wireEventSchema.options.map((schema) => schema.shape.ev.value);
      const actual = kinds.filter((kind) => invalidationPlanFor({ ev: kind, data: {} } as WireEvent)
        .invalidate.some((key) => key[0] === root));
      expect(new Set(actual)).toEqual(new Set(CONVERSATION_LIST_KINDS));
      expect(actual).toHaveLength(CONVERSATION_LIST_KINDS.length);
    },
  );

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
