// @vitest-environment node
//
// The add menu as a projection of the registry, driven through `registerAvailableBuiltinCards` rather than a fixture registry.

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

  it('keeps registration order, so terminal precedes codex as the built-in order says', () => {
    const types = builtinMenu().map((entry) => entry.type);
    expect(types.indexOf('terminal')).toBeLessThan(types.indexOf('codex'));
  });

  it('never offers a kind only the kernel may mint', () => {
    const types = builtinMenu().map((entry) => entry.type);
    expect(types).not.toContain('planner');
    expect(types).not.toContain('track-report');
  });

  /* The built-ins that exercise the two exclusions (`planner`, `track-report`) declare no `addPanel` at all, so they are filtered a step earlier; each arm of the `mode` check needs its own fixture. */
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
  /* The fixture types are deliberately not in `CardDataMap`; the cast is what a registry of heterogeneous entries costs at a test's boundary. */
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

  it('keeps a generic entry that asked to be in the menu', () => {
    const registry = createCardRegistry();
    registry.register(declaring('generic'));
    expect(cardAddMenuEntries(registry).map((entry) => entry.type)).toEqual(['fixture-generic']);
  });

  it('gives a fieldless kind an empty field list, not undefined', () => {
    const terminal = builtinMenu().find((entry) => entry.type === 'terminal');
    expect(terminal?.fields).toEqual([]);
  });
});
