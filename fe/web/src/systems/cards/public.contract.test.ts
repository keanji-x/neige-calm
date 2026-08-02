import { describe, expect, expectTypeOf, it } from 'vitest';

import type {
  CardController,
  CardDataMap,
  CardEntry,
  CardHostCapabilities,
  RegisteredCard,
} from './public.js';
import { createCardHost, createCardRegistry } from './public.js';

declare module './registry.js' {
  interface CardDataMap {
    contractAlpha: { type: 'contract-alpha'; id: string; payload: { value: number } };
    contractBeta: { type: 'contract-beta'; id: string; payload: { value: string } };
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
    fromKernel: (card) =>
      card.kind === 'shared-kernel' ? { type, id: card.id, payload: { value: type === 'contract-alpha' ? 1 : 'b' } } : null,
    createController,
  } as CardEntry;
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
  });
});
