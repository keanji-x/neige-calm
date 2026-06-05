import { describe, expect, it, beforeEach } from 'vitest';
import type { KernelCard } from '../api/wire';
import {
  assertRouterCreateAllowed,
  CatalogCreateNotImplemented,
  KernelMintedOnlyCreateNotAllowed,
} from '../app/router';
import type { WaveCardData } from '../types';
import {
  __resetRegistryForTest,
  adaptKernelCard,
  addPanelEntries,
  getEntry,
  registerCard,
  type CardEntry,
} from './registry';

declare module '../types' {
  interface WaveCardDataMap {
    'test-exact': TestExactCardData;
    'test-prefix': TestPrefixCardData;
    'test-prefix-long': TestPrefixLongCardData;
    'test-legacy': TestLegacyCardData;
    'test-atomic': TestAtomicCardData;
    'test-catalog': TestCatalogCardData;
    'test-kernel-only': TestKernelOnlyCardData;
    'test-missing': TestMissingCardData;
  }
}

interface TestExactCardData {
  type: 'test-exact';
  id: string;
}
interface TestPrefixCardData {
  type: 'test-prefix';
  id: string;
}
interface TestPrefixLongCardData {
  type: 'test-prefix-long';
  id: string;
}
interface TestLegacyCardData {
  type: 'test-legacy';
  id: string;
}
interface TestAtomicCardData {
  type: 'test-atomic';
  id: string;
}
interface TestCatalogCardData {
  type: 'test-catalog';
  id: string;
}
interface TestKernelOnlyCardData {
  type: 'test-kernel-only';
  id: string;
}
interface TestMissingCardData {
  type: 'test-missing';
  id: string;
}

function card(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'k1',
    wave_id: 'w1',
    kind: 'test-kind',
    sort: 0,
    payload: {},
    deletable: true,
    created_at: 1,
    updated_at: 2,
    ...over,
  };
}

function entry<T extends WaveCardData>(
  over: Partial<CardEntry<T>> & Pick<CardEntry<T>, 'type'>,
): CardEntry<T> {
  return {
    Component: () => null,
    defaultSize: { w: 1, h: 1, minW: 1, minH: 1 },
    title: () => String(over.type),
    accessibleName: () => `Accessible ${String(over.type)}`,
    create: { mode: 'kernel-minted-only' },
    ...over,
  } as CardEntry<T>;
}

beforeEach(() => {
  __resetRegistryForTest();
});

describe('card registry claims', () => {
  it('dispatches exact claims before prefix claims', () => {
    registerCard(
      entry<TestPrefixCardData>({
        type: 'test-prefix',
        claim: { mode: 'prefix', prefix: 'ui://' },
        create: { mode: 'catalog', catalog: 'plugin-views' },
        fromKernel: (k) =>
          k.kind.startsWith('ui://') ? { type: 'test-prefix', id: k.id } : null,
      }),
    );
    registerCard(
      entry<TestExactCardData>({
        type: 'test-exact',
        claim: { mode: 'exact', kind: 'ui://plugin/view' },
        create: {
          mode: 'generic',
          buildPayload: () => ({}),
        },
        fromKernel: (k) =>
          k.kind === 'ui://plugin/view' ? { type: 'test-exact', id: k.id } : null,
      }),
    );

    expect(adaptKernelCard(card({ kind: 'ui://plugin/view' }))).toEqual({
      type: 'test-exact',
      id: 'k1',
    });
  });

  it('uses longest-prefix-wins for prefix claims', () => {
    registerCard(
      entry<TestPrefixCardData>({
        type: 'test-prefix',
        claim: { mode: 'prefix', prefix: 'ui://' },
        create: { mode: 'catalog', catalog: 'plugin-views' },
        fromKernel: (k) =>
          k.kind.startsWith('ui://') ? { type: 'test-prefix', id: k.id } : null,
      }),
    );
    registerCard(
      entry<TestPrefixLongCardData>({
        type: 'test-prefix-long',
        claim: { mode: 'prefix', prefix: 'ui://plugin/' },
        create: { mode: 'catalog', catalog: 'plugin-views' },
        fromKernel: (k) =>
          k.kind.startsWith('ui://plugin/')
            ? { type: 'test-prefix-long', id: k.id }
            : null,
      }),
    );

    expect(adaptKernelCard(card({ kind: 'ui://plugin/view' }))).toEqual({
      type: 'test-prefix-long',
      id: 'k1',
    });
  });

  it('falls back to legacy fromKernel scanning when no claim matches', () => {
    registerCard(
      entry<TestLegacyCardData>({
        type: 'test-legacy',
        fromKernel: (k) =>
          k.kind === 'legacy-kind' ? { type: 'test-legacy', id: k.id } : null,
      }),
    );

    expect(adaptKernelCard(card({ kind: 'legacy-kind' }))).toEqual({
      type: 'test-legacy',
      id: 'k1',
    });
  });

  it('rejects duplicate exact and prefix claims', () => {
    registerCard(
      entry<TestExactCardData>({
        type: 'test-exact',
        claim: { mode: 'exact', kind: 'same-kind' },
        create: { mode: 'generic', buildPayload: () => ({}) },
      }),
    );
    expect(() =>
      registerCard(
        entry<TestAtomicCardData>({
          type: 'test-atomic',
          claim: { mode: 'exact', kind: 'same-kind' },
          create: {
            mode: 'atomic',
            submit: async () => ({ cardId: 'created' }),
          },
        }),
      ),
    ).toThrow('DuplicateExactClaim(same-kind)');

    __resetRegistryForTest();
    registerCard(
      entry<TestPrefixCardData>({
        type: 'test-prefix',
        claim: { mode: 'prefix', prefix: 'ui://' },
        create: { mode: 'catalog', catalog: 'plugin-views' },
      }),
    );
    expect(() =>
      registerCard(
        entry<TestPrefixLongCardData>({
          type: 'test-prefix-long',
          claim: { mode: 'prefix', prefix: 'ui://' },
          create: { mode: 'catalog', catalog: 'plugin-views' },
        }),
      ),
    ).toThrow('DuplicatePrefixClaim(ui://)');
  });
});

describe('card registry metadata and create invariants', () => {
  it('registers title and accessibleName metadata', () => {
    registerCard(
      entry<TestExactCardData>({
        type: 'test-exact',
        claim: { mode: 'exact', kind: 'test-kind' },
        title: () => 'Title',
        accessibleName: () => 'Accessible name',
        create: { mode: 'generic', buildPayload: () => ({}) },
      }),
    );

    const found = getEntry('test-exact');
    expect(found?.title({ type: 'test-exact', id: 'c1' })).toBe('Title');
    expect(found?.accessibleName({ type: 'test-exact', id: 'c1' })).toBe(
      'Accessible name',
    );
  });

  it('throws when metadata or create strategy is missing', () => {
    expect(() =>
      registerCard({
        type: 'test-missing',
        Component: () => null,
        defaultSize: { w: 1, h: 1, minW: 1, minH: 1 },
        accessibleName: () => 'name',
        create: { mode: 'kernel-minted-only' },
      } as unknown as CardEntry<TestMissingCardData>),
    ).toThrow('EntryMissingMetadata(test-missing, title)');

    expect(() =>
      registerCard(
        entry<TestMissingCardData>({
          type: 'test-missing',
          create: undefined,
        }),
      ),
    ).toThrow('MissingCreateStrategy(test-missing)');
  });

  it('requires generic create to use an exact claim', () => {
    expect(() =>
      registerCard(
        entry<TestPrefixCardData>({
          type: 'test-prefix',
          claim: { mode: 'prefix', prefix: 'ui://' },
          create: { mode: 'generic', buildPayload: () => ({}) },
        }),
      ),
    ).toThrow('GenericCreateRequiresExactClaim(test-prefix)');
  });

  it('omits catalog and kernel-minted-only entries from AddPanel', () => {
    registerCard(
      entry<TestExactCardData>({
        type: 'test-exact',
        addPanel: { label: 'exact' },
        claim: { mode: 'exact', kind: 'test-kind' },
        create: { mode: 'generic', buildPayload: () => ({}) },
      }),
    );
    registerCard(
      entry<TestCatalogCardData>({
        type: 'test-catalog',
        addPanel: { label: 'catalog' },
        claim: { mode: 'prefix', prefix: 'ui://' },
        create: { mode: 'catalog', catalog: 'plugin-views' },
      }),
    );
    registerCard(
      entry<TestKernelOnlyCardData>({
        type: 'test-kernel-only',
        addPanel: { label: 'kernel' },
        create: { mode: 'kernel-minted-only' },
      }),
    );

    expect(addPanelEntries().map((item) => item.type)).toEqual(['test-exact']);
  });

  it('throws the router catalog-create contract error', () => {
    const catalogEntry = entry<TestCatalogCardData>({
      type: 'test-catalog',
      claim: { mode: 'prefix', prefix: 'ui://' },
      create: { mode: 'catalog', catalog: 'plugin-views' },
    });

    expect(() => assertRouterCreateAllowed(catalogEntry)).toThrow(
      CatalogCreateNotImplemented,
    );
    expect(() => assertRouterCreateAllowed(catalogEntry)).toThrow(
      'CatalogCreateNotImplemented',
    );
  });

  it('throws the router kernel-minted-only create contract error', () => {
    const kernelOnlyEntry = entry<TestKernelOnlyCardData>({
      type: 'test-kernel-only',
      create: { mode: 'kernel-minted-only' },
    });

    expect(() => assertRouterCreateAllowed(kernelOnlyEntry)).toThrow(
      KernelMintedOnlyCreateNotAllowed,
    );
    expect(() => assertRouterCreateAllowed(kernelOnlyEntry)).toThrow(
      'KernelMintedOnlyCreateNotAllowed',
    );
  });
});

describe('WaveCardDataMap type contract', () => {
  it('keeps WaveCardData as a discriminated union of registered map values', () => {
    const cardData: WaveCardData = { type: 'test-exact', id: 'c1' };
    expect(cardData.type).toBe('test-exact');

    // @ts-expect-error nonexistent is not a valid WaveCardData discriminant.
    if (cardData.type === 'nonexistent') {
      expect(cardData).toBe(cardData);
    }
  });
});
