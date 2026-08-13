import { describe, expect, it } from 'vitest';

import type { WireEvent } from '../api/schemas.js';
import { invalidationPlanFor } from './invalidation-plan.js';

function event(value: unknown): WireEvent {
  return value as WireEvent;
}

describe('invalidation plan behavior', () => {
  it('write-through updates only an existing cove before invalidating the list', () => {
    const value = event({ ev: 'cove.updated', data: { id: 'c1', name: 'new' } });
    expect(invalidationPlanFor(value)).toEqual({
      invalidate: [['coves']],
      remove: [],
      writeThrough: [{ key: ['coves'], mode: 'replace-existing-cove', value: value.data }],
    });
  });

  it('invalidates all four wave projections for wave updates', () => {
    expect(invalidationPlanFor(event({ ev: 'wave.updated', data: { id: 'w1', cove_id: 'c1' } }))).toEqual({
      invalidate: [['waves', 'cove', 'c1'], ['wave', 'w1'], ['wave-files', 'w1'], ['waves-range']],
      remove: [],
      writeThrough: [],
    });
  });

  it('removes deleted wave detail after invalidating its remaining projections', () => {
    expect(invalidationPlanFor(event({ ev: 'wave.deleted', data: { id: 'w1', cove_id: 'c1' } }))).toEqual({
      invalidate: [['waves', 'cove', 'c1'], ['overlays', 'wave'], ['waves-range']],
      remove: [['wave', 'w1']],
      writeThrough: [],
    });
  });

  it('invalidates card mutations immediately without suppression or debounce state', () => {
    expect(invalidationPlanFor(event({ ev: 'card.added', data: { wave_id: 'w1' } }))).toEqual({
      invalidate: [['wave', 'w1'], ['wave-files', 'w1']],
      remove: [],
      writeThrough: [],
    });
  });

  it('resolves runtime projections through card ownership', () => {
    expect(invalidationPlanFor(
      event({ ev: 'runtime.started', data: { card_id: 'card-1' } }),
      { findWaveOwningCard: () => 'wave-1' },
    )).toEqual({
      invalidate: [['wave', 'wave-1'], ['overlays', 'card'], ['wave-files', 'wave-1'], ['wave-report', 'wave-1']],
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
    expect(invalidationPlanFor(event({ ev: 'codex.hook', data: { wave_id: 'direct' } }), context).invalidate)
      .toEqual([['wave-files', 'direct'], ['wave-report', 'direct']]);
    expect(invalidationPlanFor(event({ ev: 'codex.hook', data: { card_id: 'card-1' } }), context).invalidate)
      .toEqual([['wave-files', 'wave-1'], ['wave-report', 'wave-1']]);
    expect(invalidationPlanFor(event({ ev: 'codex.hook', data: {} }), context).invalidate)
      .toEqual([['wave-files'], ['wave-report']]);
  });

  it('invalidates terminal runtime projection through card ownership', () => {
    expect(invalidationPlanFor(
      event({ ev: 'terminal.deleted', data: { card_id: 'card-1' } }),
      { findWaveOwningCard: () => 'wave-1' },
    ).invalidate).toEqual([['wave-files', 'wave-1'], ['wave-report', 'wave-1']]);
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
