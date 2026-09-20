import { describe, expect, it } from 'vitest';

import type { CardWire } from '../../../../../core/domain/track.ts';
import { createCardRegistry } from '../registry.js';
import { partitionTrackCards } from './headless-filter.js';
import { registerAvailableBuiltinCards } from './register.js';

function wire(overrides: Partial<CardWire> & Pick<CardWire, 'id' | 'kind'>): CardWire {
  return {
    track_id: 'w1', title: null, sort: 0, payload: null,
    deletable: true, created_at: 0, updated_at: 0, ...overrides,
  };
}

function bootedRegistry() {
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  return registry;
}

describe('headless card filtering', () => {
  it('[INV-CARD-226] binds originalIndex before filtering, so surviving cards still address the wire array', () => {
    const cards = [
      wire({ id: 'planner-1', kind: 'codex', payload: { planner_harness: true } }),
      wire({ id: 'report', kind: 'track-report' }),
      wire({ id: 'term-1', kind: 'terminal' }),
      wire({ id: 'term-2', kind: 'terminal' }),
    ];
    const { visible, unknown } = partitionTrackCards(bootedRegistry(), cards);

    // `originalIndex` must index the raw card list, not the visible one: removing by display index would delete the headless planner harness.
    expect(visible.map((slot) => [slot.wire.id, slot.originalIndex])).toEqual([['term-1', 2], ['term-2', 3]]);
    expect(unknown).toEqual([]);
    for (const slot of visible) expect(cards[slot.originalIndex]).toBe(slot.wire);
  });

  it('[INV-CARD-226] drops resolved planner and track-report cards from both branches', () => {
    const { visible, unknown } = partitionTrackCards(bootedRegistry(), [
      wire({ id: 'planner-1', kind: 'codex', payload: { planner_harness: true } }),
      wire({ id: 'report', kind: 'track-report' }),
    ]);
    expect(visible).toEqual([]);
    expect(unknown).toEqual([]);
  });

  it('[INV-CARD-226] filters raw track-report kinds out of the unknown branch defensively', () => {
    const bare = createCardRegistry();
    const { visible, unknown } = partitionTrackCards(bare, [
      wire({ id: 'report', kind: 'track-report' }),
      wire({ id: 'term-1', kind: 'terminal' }),
    ]);
    expect(visible).toEqual([]);
    expect(unknown.map((slot) => slot.wire.id)).toEqual(['term-1']);
    expect(unknown[0]?.originalIndex).toBe(1);
  });

  it('[INV-CARD-226] keeps ordinary codex cards visible, never headless', () => {
    const { visible, unknown } = partitionTrackCards(bootedRegistry(), [
      wire({ id: 'codex-1', kind: 'codex', payload: {} }),
      wire({ id: 'codex-2', kind: 'codex', payload: null }),
      wire({ id: 'codex-3', kind: 'codex', payload: { planner_harness: false } }),
    ]);
    expect(visible.map((slot) => slot.wire.id)).toEqual(['codex-1', 'codex-2', 'codex-3']);
    expect(visible.map((slot) => slot.card.type)).toEqual(['codex', 'codex', 'codex']);
    expect(visible.map((slot) => slot.originalIndex)).toEqual([0, 1, 2]);
    expect(unknown).toEqual([]);
  });

  it('[INV-CARD-226] accepts the known gap: a payload-less planner card cannot be recognised as headless', () => {
    const bare = createCardRegistry();
    const { unknown } = partitionTrackCards(bare, [wire({ id: 'planner-1', kind: 'codex', payload: null })]);
    expect(unknown.map((slot) => slot.wire.id)).toEqual(['planner-1']);
  });

  it('routes cards with a surface into the visible branch with their wire and index', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    registry.register({
      type: 'surface-fixture',
      component: () => null,
      defaultSize: { w: 4, h: 6, minW: 3, minH: 3 },
      title: () => 'fixture',
      accessibleName: () => 'fixture',
      create: { mode: 'kernel-minted-only' },
      fromKernel: (raw) => (raw.kind === 'surface' ? { type: 'surface-fixture', id: raw.id } : null),
    });
    const cards = [
      wire({ id: 'report', kind: 'track-report' }),
      wire({ id: 'surface-1', kind: 'surface' }),
      wire({ id: 'term-1', kind: 'terminal' }),
    ];
    const { visible, unknown } = partitionTrackCards(registry, cards);
    expect(visible.map((slot) => [slot.card.type, slot.wire.id, slot.originalIndex]))
      .toEqual([['surface-fixture', 'surface-1', 1], ['terminal', 'term-1', 2]]);
    expect(unknown).toEqual([]);
  });

  it('preserves arrival order and does not sort', () => {
    const cards = [
      wire({ id: 'b', kind: 'terminal', sort: 9 }),
      wire({ id: 'a', kind: 'terminal', sort: 1 }),
    ];
    const { visible } = partitionTrackCards(bootedRegistry(), cards);
    expect(visible.map((slot) => slot.wire.id)).toEqual(['b', 'a']);
  });

  it('returns empty branches for an empty track', () => {
    const { visible, unknown } = partitionTrackCards(bootedRegistry(), []);
    expect(visible).toEqual([]);
    expect(unknown).toEqual([]);
  });
});

declare module '../registry.js' {
  interface CardDataMap {
    surfaceFixture: { readonly type: 'surface-fixture'; readonly id: string };
  }
}
