import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import type { PlannerCard } from './planner.js';
import { isPlannerHarnessPayload, PLANNER_CARD_ENTRY } from './planner.js';
import { registerAvailableBuiltinCards } from './register.js';

const callComponent = (entry: typeof PLANNER_CARD_ENTRY) =>
  (entry.component as unknown as (props: unknown) => unknown)({});

describe('planner card entry', () => {
  it('[INV-CARD-182] recognises a planner harness only by the planner_harness discriminator', () => {
    expect(PLANNER_CARD_ENTRY.fromKernel?.({ id: 'c1', kind: 'codex', payload: { planner_harness: true } }))
      .toEqual({ type: 'planner', id: 'c1' });
    expect(PLANNER_CARD_ENTRY.fromKernel?.({ id: 'c2', kind: 'codex', payload: { planner_harness: true, title: 'x' } }))
      .toEqual({ type: 'planner', id: 'c2' });
  });

  it('[INV-CARD-182] refuses an ordinary codex card, so widening the predicate to kind alone is red', () => {
    // A `kind === 'codex'` predicate would make every ordinary codex card a headless planner.
    for (const payload of [{}, { planner_harness: false }, { planner_harness: 'true' }, { planner_harness: 1 }]) {
      expect(PLANNER_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload }), JSON.stringify(payload)).toBeNull();
    }
  });

  it('[INV-CARD-182] narrows non-object payloads instead of throwing', () => {
    for (const payload of [null, undefined, 'planner_harness', 7, true, []]) {
      expect(() => PLANNER_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload })).not.toThrow();
      expect(PLANNER_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload })).toBeNull();
    }
    expect(isPlannerHarnessPayload(null)).toBe(false);
    expect(isPlannerHarnessPayload({ planner_harness: true })).toBe(true);
  });

  it('[INV-CARD-182] never claims a kernel kind other than codex', () => {
    expect(PLANNER_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'terminal', payload: { planner_harness: true } })).toBeNull();
    expect(PLANNER_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'planner', payload: { planner_harness: true } })).toBeNull();
  });

  it('[INV-CARD-181] is headless, 1x1 and kernel-minted only', () => {
    expect(callComponent(PLANNER_CARD_ENTRY)).toBeNull();
    expect(PLANNER_CARD_ENTRY.defaultSize).toEqual({ w: 1, h: 1, minW: 1, minH: 1 });
    expect(PLANNER_CARD_ENTRY.create).toEqual({ mode: 'kernel-minted-only' });
  });

  it('[INV-CARD-181] takes no claim, so it stays on the insertion-ordered fallback scan', () => {
    expect((PLANNER_CARD_ENTRY as CardEntry<PlannerCard>).claim).toBeUndefined();
  });

  it('resolves a planner harness through a really booted registry, and only a harness', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.resolve({ id: 'c1', kind: 'codex', payload: { planner_harness: true } })?.type).toBe('planner');
    expect(registry.resolve({ id: 'c2', kind: 'codex', payload: {} })?.type).toBe('codex');
  });
});
