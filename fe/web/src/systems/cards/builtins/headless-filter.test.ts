import { describe, expect, it } from 'vitest';

import type { CardWire } from '../../../../../core/domain/wave.ts';
import { createCardRegistry } from '../registry.js';
import { partitionWaveCards } from './headless-filter.js';
import { registerAvailableBuiltinCards } from './register.js';

function wire(overrides: Partial<CardWire> & Pick<CardWire, 'id' | 'kind'>): CardWire {
  return {
    wave_id: 'w1', title: null, sort: 0, payload: null,
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
      wire({ id: 'spec-1', kind: 'codex', payload: { spec_harness: true } }),
      wire({ id: 'report', kind: 'wave-report' }),
      wire({ id: 'term-1', kind: 'terminal' }),
      wire({ id: 'term-2', kind: 'terminal' }),
    ];
    const { visible, unknown } = partitionWaveCards(bootedRegistry(), cards);

    // Two headless cards sit in front. A display index would report 0 and 1
    // here, and `detail.cards[0]` is the spec card — removing `term-1` would
    // delete the harness instead.
    expect(unknown.map((slot) => [slot.wire.id, slot.originalIndex])).toEqual([['term-1', 2], ['term-2', 3]]);
    expect(visible).toEqual([]);
    for (const slot of unknown) expect(cards[slot.originalIndex]).toBe(slot.wire);
  });

  it('[INV-CARD-226] drops resolved spec and wave-report cards from both branches', () => {
    const { visible, unknown } = partitionWaveCards(bootedRegistry(), [
      wire({ id: 'spec-1', kind: 'codex', payload: { spec_harness: true } }),
      wire({ id: 'report', kind: 'wave-report' }),
    ]);
    expect(visible).toEqual([]);
    expect(unknown).toEqual([]);
  });

  it('[INV-CARD-226] filters raw wave-report kinds out of the unknown branch defensively', () => {
    // With no wave-report adapter registered the card would otherwise surface
    // as a diagnosable unknown slot; the report is never a panel.
    const bare = createCardRegistry();
    const { visible, unknown } = partitionWaveCards(bare, [
      wire({ id: 'report', kind: 'wave-report' }),
      wire({ id: 'term-1', kind: 'terminal' }),
    ]);
    expect(visible).toEqual([]);
    expect(unknown.map((slot) => slot.wire.id)).toEqual(['term-1']);
    expect(unknown[0]?.originalIndex).toBe(1);
  });

  it('[INV-CARD-226] keeps an ordinary codex card in the unknown branch until a codex adapter lands', () => {
    // The counter-example that guards the spec predicate end to end: if spec
    // matched on `kind === 'codex'` alone this card would be resolved headless
    // and vanish from the wave entirely.
    const { visible, unknown } = partitionWaveCards(bootedRegistry(), [
      wire({ id: 'codex-1', kind: 'codex', payload: {} }),
      wire({ id: 'codex-2', kind: 'codex', payload: null }),
      wire({ id: 'codex-3', kind: 'codex', payload: { spec_harness: false } }),
    ]);
    expect(visible).toEqual([]);
    expect(unknown.map((slot) => slot.wire.id)).toEqual(['codex-1', 'codex-2', 'codex-3']);
    expect(unknown.map((slot) => slot.originalIndex)).toEqual([0, 1, 2]);
  });

  it('[INV-CARD-226] accepts the known gap: a payload-less spec card cannot be recognised as headless', () => {
    const bare = createCardRegistry();
    const { unknown } = partitionWaveCards(bare, [wire({ id: 'spec-1', kind: 'codex', payload: null })]);
    expect(unknown.map((slot) => slot.wire.id)).toEqual(['spec-1']);
  });

  it('routes cards with a surface into the visible branch with their wire and index', () => {
    // No built-in has a surface in S1, so this exercises the branch with a
    // registered adapter of its own rather than asserting an empty list.
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
      wire({ id: 'report', kind: 'wave-report' }),
      wire({ id: 'surface-1', kind: 'surface' }),
      wire({ id: 'term-1', kind: 'terminal' }),
    ];
    const { visible, unknown } = partitionWaveCards(registry, cards);
    expect(visible.map((slot) => [slot.card.type, slot.wire.id, slot.originalIndex]))
      .toEqual([['surface-fixture', 'surface-1', 1]]);
    expect(unknown.map((slot) => [slot.wire.id, slot.originalIndex])).toEqual([['term-1', 2]]);
  });

  it('preserves arrival order and does not sort', () => {
    const cards = [
      wire({ id: 'b', kind: 'terminal', sort: 9 }),
      wire({ id: 'a', kind: 'terminal', sort: 1 }),
    ];
    const { unknown } = partitionWaveCards(bootedRegistry(), cards);
    expect(unknown.map((slot) => slot.wire.id)).toEqual(['b', 'a']);
  });

  it('returns empty branches for an empty wave', () => {
    const { visible, unknown } = partitionWaveCards(bootedRegistry(), []);
    expect(visible).toEqual([]);
    expect(unknown).toEqual([]);
  });
});

declare module '../registry.js' {
  interface CardDataMap {
    surfaceFixture: { readonly type: 'surface-fixture'; readonly id: string };
  }
}
