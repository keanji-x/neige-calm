import { describe, expect, expectTypeOf, it, vi } from 'vitest';

import type {
  CardController,
  CardDataMap,
  CardEntry,
  CardHostCapabilities,
  KernelCardInput,
  RegisteredCard,
} from './public.js';
import { createCardHost, createCardRegistry } from './public.js';

declare module './registry.js' {
  interface CardDataMap {
    contractAlpha: { type: 'contract-alpha'; id: string; payload: { value: number } };
    contractBeta: { type: 'contract-beta'; id: string; payload: { value: string } };
    contractPrefix: { type: 'contract-prefix'; id: string; payload: null };
    contractDeep: { type: 'contract-deep'; id: string; payload: null };
    contractFallback: { type: 'contract-fallback'; id: string; payload: null };
  }
}

const component = () => null;
const size = Object.freeze({ w: 4, h: 6, minW: 3, minH: 3 });
const create = Object.freeze({ mode: 'kernel-minted-only' } as const);

function entry(
  type: 'contract-alpha' | 'contract-beta',
  createController?: CardEntry['createController'],
): CardEntry {
  return {
    type,
    component,
    defaultSize: size,
    title: () => type,
    accessibleName: () => type,
    create,
    fromKernel: (card) => {
      if (card.kind !== 'shared-kernel') return null;
      return type === 'contract-alpha'
        ? { type: 'contract-alpha', id: card.id, payload: { value: 1 } }
        : { type: 'contract-beta', id: card.id, payload: { value: 'b' } };
    },
    createController,
  } satisfies CardEntry;
}

describe('cards public contract', () => {
  it('[GATE-CARD-083/084] re-exports the augmented complete card types', () => {
    expectTypeOf<CardDataMap['contractAlpha']>().toEqualTypeOf<{
      type: 'contract-alpha'; id: string; payload: { value: number };
    }>();
    expectTypeOf<Extract<RegisteredCard, { type: 'contract-beta' }>>().toEqualTypeOf<{
      type: 'contract-beta'; id: string; payload: { value: string };
    }>();
  });

  it('[INV-CARD-073/225] preserves fallback full-scan insertion order', () => {
    const registry = createCardRegistry();
    registry.register(entry('contract-alpha'));
    registry.register(entry('contract-beta'));
    expect(registry.resolve({ id: 'k1', kind: 'shared-kernel', payload: null })?.type).toBe('contract-alpha');
  });

  it('[INV-CARD-106] visibility changes never unmount a mounted card', () => {
    const registry = createCardRegistry();
    let disposals = 0;
    registry.register(entry('contract-alpha', () => ({ dispose: () => { disposals += 1; } })));
    const host = createCardHost(registry);
    const mounted = host.mount({ type: 'contract-alpha', id: 'c1', payload: { value: 1 } });
    mounted.host.setVisible(false);
    expect(host.resolve('c1')).toBe(mounted.card);
    expect(disposals).toBe(0);
    mounted.unmount();
    expect(disposals).toBe(1);
  });

  it('[INV-CARD-095/102] exposes lifecycle writing only on the host side', () => {
    expectTypeOf<CardHostCapabilities['lifecycle']>().not.toHaveProperty('setVisible');
    const compileOnly = false as boolean;
    if (compileOnly) {
      const card = null as unknown as CardHostCapabilities;
      // @ts-expect-error -- delete this whole line: cards receive a read-only lifecycle view.
      void card.lifecycle.setVisible;
      const controller = null as unknown as CardController;
      void controller;
    }
    const registry = createCardRegistry();
    registry.register(entry('contract-alpha'));
    const mounted = createCardHost(registry).mount({ type: 'contract-alpha', id: 'readonly', payload: { value: 1 } });
    expect((mounted.card.lifecycle as unknown as Record<string, unknown>).setVisible).toBeUndefined();
    expect(Object.isFrozen(mounted.card.lifecycle)).toBe(true);
    expect(Object.keys(mounted.card.lifecycle).sort()).toEqual(['getSnapshot', 'subscribe']);
  });

  it('[INV-CARD-073] dispatches exact, longest prefix, then untried fallback', () => {
    const registry = createCardRegistry();
    const exact = vi.fn((raw: KernelCardInput) => raw.kind === 'k' ? { type: 'contract-alpha' as const, id: raw.id, payload: { value: 1 } } : null);
    const short = vi.fn((raw: KernelCardInput) => raw.id === '3' ? null : { type: 'contract-prefix' as const, id: raw.id, payload: null });
    const deep = vi.fn((raw: KernelCardInput) => raw.id === '3' ? null : { type: 'contract-deep' as const, id: raw.id, payload: null });
    const fallback = vi.fn((raw: KernelCardInput) => raw.id === '3' ? null : { type: 'contract-fallback' as const, id: raw.id, payload: null });
    const make = (type: CardEntry['type'], claim: CardEntry['claim'], fromKernel: NonNullable<CardEntry['fromKernel']>) => ({
      component, defaultSize: size, title: () => type, accessibleName: () => type, create, type, claim, fromKernel,
    }) satisfies CardEntry;
    registry.register(make('contract-alpha', { mode: 'exact', kind: 'k' }, exact));
    registry.register(make('contract-prefix', { mode: 'prefix', prefix: 'k' }, short));
    registry.register(make('contract-deep', { mode: 'prefix', prefix: 'k/deep' }, deep));
    registry.register(make('contract-fallback', undefined, fallback));

    expect(registry.resolve({ id: '1', kind: 'k', payload: null })?.type).toBe('contract-alpha');
    expect(short).not.toHaveBeenCalled();
    expect(registry.resolve({ id: '2', kind: 'k/deep/x', payload: null })?.type).toBe('contract-deep');
    exact.mockClear();
    exact.mockReturnValue(null);
    expect(registry.resolve({ id: '3', kind: 'k', payload: null })).toBeNull();
    expect(exact).toHaveBeenCalledOnce();
  });
});
