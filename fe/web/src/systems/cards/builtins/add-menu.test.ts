// @vitest-environment node
//
// The add menu as a projection of the registry.
//
// Driven through `registerAvailableBuiltinCards` rather than a hand-built
// registry: the claim under test is about what *this build* offers, and a
// fixture registry would only prove that the filter works on rows the fixture
// chose. The kinds a kernel-minted-only entry must never surface (`spec`,
// `wave-report`) are real entries here, not stand-ins.

import { describe, expect, it } from 'vitest';

import { cardAddMenuEntries, createCardRegistry, type CardEntry } from '../public.js';
import { registerAvailableBuiltinCards } from './register.js';

function builtinMenu() {
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  return cardAddMenuEntries(registry);
}

describe('cardAddMenuEntries', () => {
  it('offers exactly the built-ins that declared an add-panel entry', () => {
    expect(builtinMenu().map((entry) => [entry.type, entry.label]))
      .toEqual([['terminal', 'terminal'], ['codex', 'codex'], ['file-viewer', 'file']]);
  });

  /*
   * The menu's order is `BUILTIN_CARD_ORDER`, inherited through registration
   * order rather than restated — a second ordering table here is exactly the
   * kind of duplicate that drifts. Asserted as the full list above; this case
   * pins the *reason* by naming the two kinds whose relative order would flip
   * if the projection ever sorted by anything of its own (label, say).
   */
  it('keeps registration order, so terminal precedes codex as the built-in order says', () => {
    const types = builtinMenu().map((entry) => entry.type);
    expect(types.indexOf('terminal')).toBeLessThan(types.indexOf('codex'));
  });

  it('never offers a kind only the kernel may mint', () => {
    const types = builtinMenu().map((entry) => entry.type);
    expect(types).not.toContain('spec');
    expect(types).not.toContain('wave-report');
  });

  /*
   * The two exclusions are independent rules and fail independently, so each
   * gets a single-violation fixture: an entry that declares an `addPanel` and
   * would be offered but for its create strategy. Without these, deleting
   * either arm of the `mode` check stays green — the built-ins that exercise
   * them (`spec`, `wave-report`) declare no `addPanel` at all, so they are
   * filtered a step earlier and prove nothing about this check.
   */
  const declaring = (mode: 'kernel-minted-only' | 'catalog' | 'generic'): CardEntry => ({
    type: `fixture-${mode}`,
    component: () => null,
    defaultSize: Object.freeze({ w: 4, h: 6, minW: 3, minH: 3 }),
    claim: Object.freeze({ mode: 'exact', kind: `fixture-${mode}` } as const),
    title: () => 'Fixture',
    accessibleName: () => 'Fixture',
    create: mode === 'generic'
      ? Object.freeze({ mode: 'generic' as const, buildPayload: () => ({}) })
      : mode === 'catalog'
        ? Object.freeze({ mode: 'catalog' as const, catalog: 'fixture' })
        : Object.freeze({ mode: 'kernel-minted-only' as const }),
    addPanel: Object.freeze({ label: 'fixture' }),
  /* The fixture types are deliberately not in `CardDataMap` — a fixture kind
     that declared itself there would be a card kind of the product. The cast is
     what a registry of heterogeneous entries costs at a test's boundary. */
  } as unknown as CardEntry);

  it('drops a kernel-minted-only entry that asked to be in the menu', () => {
    const registry = createCardRegistry();
    registry.register(declaring('kernel-minted-only'));
    expect(cardAddMenuEntries(registry)).toEqual([]);
  });

  it('drops a catalog entry that asked to be in the menu', () => {
    const registry = createCardRegistry();
    registry.register(declaring('catalog'));
    expect(cardAddMenuEntries(registry)).toEqual([]);
  });

  /* The positive half of the same pair: without it, a filter that dropped
     everything would satisfy both cases above. */
  it('keeps a generic entry that asked to be in the menu', () => {
    const registry = createCardRegistry();
    registry.register(declaring('generic'));
    expect(cardAddMenuEntries(registry).map((entry) => entry.type)).toEqual(['fixture-generic']);
  });

  it('gives a fieldless kind an empty field list, not undefined', () => {
    const terminal = builtinMenu().find((entry) => entry.type === 'terminal');
    // The caller branches on `fields.length === 0` to create without a form;
    // `undefined` there would throw on a gesture that is supposed to be the
    // cheapest one in the menu.
    expect(terminal?.fields).toEqual([]);
  });
});
