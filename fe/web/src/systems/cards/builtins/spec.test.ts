import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import type { SpecCard } from './spec.js';
import { isSpecHarnessPayload, SPEC_CARD_ENTRY } from './spec.js';
import { registerAvailableBuiltinCards } from './register.js';

const callComponent = (entry: typeof SPEC_CARD_ENTRY) =>
  (entry.component as unknown as (props: unknown) => unknown)({});

describe('spec card entry', () => {
  it('[INV-CARD-182] recognises a spec harness only by the spec_harness discriminator', () => {
    expect(SPEC_CARD_ENTRY.fromKernel?.({ id: 'c1', kind: 'codex', payload: { spec_harness: true } }))
      .toEqual({ type: 'spec', id: 'c1' });
    // Extra payload fields are irrelevant — the discriminator alone decides.
    expect(SPEC_CARD_ENTRY.fromKernel?.({ id: 'c2', kind: 'codex', payload: { spec_harness: true, title: 'x' } }))
      .toEqual({ type: 'spec', id: 'c2' });
  });

  it('[INV-CARD-182] refuses an ordinary codex card, so widening the predicate to kind alone is red', () => {
    // This is the whole reason the predicate is two clauses. If `fromKernel`
    // were `kind === 'codex'`, every ordinary codex card in production would
    // resolve to a headless spec and disappear from the wave.
    for (const payload of [{}, { spec_harness: false }, { spec_harness: 'true' }, { spec_harness: 1 }]) {
      expect(SPEC_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload }), JSON.stringify(payload)).toBeNull();
    }
  });

  it('[INV-CARD-182] narrows non-object payloads instead of throwing', () => {
    for (const payload of [null, undefined, 'spec_harness', 7, true, []]) {
      expect(() => SPEC_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload })).not.toThrow();
      expect(SPEC_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload })).toBeNull();
    }
    expect(isSpecHarnessPayload(null)).toBe(false);
    expect(isSpecHarnessPayload({ spec_harness: true })).toBe(true);
  });

  it('[INV-CARD-182] never claims a kernel kind other than codex', () => {
    expect(SPEC_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'terminal', payload: { spec_harness: true } })).toBeNull();
    expect(SPEC_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'spec', payload: { spec_harness: true } })).toBeNull();
  });

  it('[INV-CARD-181] is headless, 1x1 and kernel-minted only', () => {
    expect(callComponent(SPEC_CARD_ENTRY)).toBeNull();
    expect(SPEC_CARD_ENTRY.defaultSize).toEqual({ w: 1, h: 1, minW: 1, minH: 1 });
    expect(SPEC_CARD_ENTRY.create).toEqual({ mode: 'kernel-minted-only' });
  });

  it('[INV-CARD-181] takes no claim, so it stays on the insertion-ordered fallback scan', () => {
    // An exact claim on `'codex'` would let spec pre-empt the codex entry that
    // lands in S4a; the shared kernel kind is resolved by scan order instead.
    // Read through the interface: the entry literal is checked with `satisfies`
    // so registration can require `headless`, which means the constant's own
    // type only lists the members it declares.
    expect((SPEC_CARD_ENTRY as CardEntry<SpecCard>).claim).toBeUndefined();
  });

  it('resolves a spec harness through a really booted registry', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.resolve({ id: 'c1', kind: 'codex', payload: { spec_harness: true } })?.type).toBe('spec');
    expect(registry.resolve({ id: 'c2', kind: 'codex', payload: {} })).toBeNull();
  });
});
