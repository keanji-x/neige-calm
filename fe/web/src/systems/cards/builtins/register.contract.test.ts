import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import { BUILTIN_CARD_ORDER, registerAvailableBuiltinCards } from './register.js';
import { SPEC_CARD_ENTRY } from './spec.js';
import { WAVE_REPORT_CARD_ENTRY } from './wave-report.js';

declare module '../registry.js' {
  interface CardDataMap {
    bootCodexFixture: { readonly type: 'boot-codex'; readonly id: string };
  }
}

const LANDED_IN_S1 = ['spec', 'wave-report'] as const;

describe('builtin card composition contract', () => {
  it('[INV-CARD-225] pins the eight-item order tuple', () => {
    // Not a set and not sorted: the registry's fallback scan runs in insertion
    // order, so this literal *is* the resolution semantics. Changing an entry
    // or dropping one changes which adapter claims a shared kernel kind.
    expect([...BUILTIN_CARD_ORDER]).toEqual([
      'terminal', 'codex', 'spec', 'claude', 'wave-report', 'file-viewer', 'iframe', 'plugin-iframe',
    ]);
    expect(BUILTIN_CARD_ORDER).toHaveLength(8);
    expect(new Set(BUILTIN_CARD_ORDER).size).toBe(8);
  });

  it('registers only the entries that exist, with no placeholders for the six that do not', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.entries().map((entry) => entry.type)).toEqual([...LANDED_IN_S1]);
    for (const type of BUILTIN_CARD_ORDER) {
      if ((LANDED_IN_S1 as readonly string[]).includes(type)) expect(registry.get(type)).toBeDefined();
      // A no-op/unknown placeholder would satisfy "eight entries" and then
      // swallow real kernel cards into a card that renders nothing.
      else expect(registry.get(type), `${type} must be absent, not a placeholder`).toBeUndefined();
    }
    expect(registry.get('spec')).toBe(SPEC_CARD_ENTRY);
    expect(registry.get('wave-report')).toBe(WAVE_REPORT_CARD_ENTRY);
  });

  it('[INV-CARD-225] keeps the landed entries in tuple-relative order across the skipped holes', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const registered = registry.entries().map((entry) => entry.type);
    const tupleIndexes = registered.map((type) => BUILTIN_CARD_ORDER.indexOf(type as never));
    expect(tupleIndexes).not.toContain(-1);
    expect([...tupleIndexes]).toEqual([...tupleIndexes].sort((left, right) => left - right));
    // spec (index 2) and wave-report (index 4) are separated by `claude`, which
    // is skipped: the relative order must survive the hole.
    expect(tupleIndexes).toEqual([2, 4]);
  });

  it('[INV-CARD-180] leaves the shared codex kind to a codex adapter first, then falls back to spec', () => {
    // S1 has no production codex entry (that is S4a), so the refusing half is a
    // fixture. What is real here is SPEC_CARD_ENTRY and the registry's
    // insertion-ordered fallback scan.
    const registry = createCardRegistry();
    const codexFixture: CardEntry<{ readonly type: 'boot-codex'; readonly id: string }> = {
      type: 'boot-codex',
      component: () => null,
      defaultSize: { w: 4, h: 6, minW: 3, minH: 3 },
      title: () => 'codex fixture',
      accessibleName: () => 'codex fixture',
      create: { mode: 'kernel-minted-only' },
      fromKernel: (raw) => (raw.kind === 'codex'
        && !(typeof raw.payload === 'object' && raw.payload !== null
          && (raw.payload as { spec_harness?: unknown }).spec_harness === true)
        ? { type: 'boot-codex', id: raw.id }
        : null),
    };
    registry.register(codexFixture);
    registry.register(SPEC_CARD_ENTRY);

    expect(
      registry.resolve({ id: 'spec', kind: 'codex', payload: { spec_harness: true } })?.type,
      'spec_harness must fall through the earlier codex adapter and resolve as spec',
    ).toBe('spec');
    expect(
      registry.resolve({ id: 'codex', kind: 'codex', payload: {} })?.type,
      'ordinary codex payload must resolve through codex before the shared-kind spec fallback',
    ).toBe('boot-codex');
  });

  it('takes no entries and keeps no state: two registries boot independently', () => {
    const first = createCardRegistry();
    const second = createCardRegistry();
    registerAvailableBuiltinCards(first);
    registerAvailableBuiltinCards(second);
    expect(second.entries().map((entry) => entry.type)).toEqual([...LANDED_IN_S1]);
    expect(registerAvailableBuiltinCards).toHaveLength(1);
  });
});
