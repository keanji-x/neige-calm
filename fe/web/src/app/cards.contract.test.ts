import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../systems/cards/public.js';
import { createCardRegistry } from '../systems/cards/public.js';
import { registerBuiltinCards } from './cards.js';

declare module '../systems/cards/registry.js' {
  interface CardDataMap {
    bootFixture: { type: 'boot-fixture'; id: string; payload: null };
    bootFixtureTwo: { type: 'boot-fixture-two'; id: string; payload: null };
    bootFixtureThree: { type: 'boot-fixture-three'; id: string; payload: null };
    bootFixtureFour: { type: 'boot-fixture-four'; id: string; payload: null };
    bootFixtureFive: { type: 'boot-fixture-five'; id: string; payload: null };
    bootFixtureSix: { type: 'boot-fixture-six'; id: string; payload: null };
    bootCodex: { type: 'boot-codex'; id: string; payload: null };
    bootSpec: { type: 'boot-spec'; id: string; payload: null };
  }
}

const base = {
  component: () => null,
  defaultSize: Object.freeze({ w: 4, h: 6, minW: 3, minH: 3 }),
  title: () => 'fixture',
  accessibleName: () => 'fixture',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
};

describe('card boot composition contract', () => {
  it('[INV-CARD-180/225] preserves codex/spec fallback semantics through real registration', () => {
    const registry = createCardRegistry();
    const fixture: CardEntry = { ...base, type: 'boot-fixture', fromKernel: () => null };
    const codex: CardEntry = {
      ...base,
      type: 'boot-codex',
      fromKernel: (raw) => raw.kind === 'codex' && !(raw.payload as { spec_harness?: boolean }).spec_harness
        ? { type: 'boot-codex', id: raw.id, payload: null }
        : null,
    };
    const spec: CardEntry = {
      ...base,
      type: 'boot-spec',
      fromKernel: (raw) => raw.kind === 'codex'
        ? { type: 'boot-spec', id: raw.id, payload: null }
        : null,
    };
    const makeFixture = (type: CardEntry['type']): CardEntry => ({ ...base, type, fromKernel: () => null });
    registerBuiltinCards(
      registry,
      fixture,
      codex,
      spec,
      makeFixture('boot-fixture-two'),
      makeFixture('boot-fixture-three'),
      makeFixture('boot-fixture-four'),
      makeFixture('boot-fixture-five'),
      makeFixture('boot-fixture-six'),
    );

    expect(
      registry.resolve({ id: 'spec', kind: 'codex', payload: { spec_harness: true } })?.type,
      'spec_harness must fall through the earlier codex adapter and resolve as spec',
    ).toBe('boot-spec');
    expect(
      registry.resolve({ id: 'codex', kind: 'codex', payload: {} })?.type,
      'ordinary codex payload must resolve through codex before the shared-kind spec fallback',
    ).toBe('boot-codex');
  });
});
