import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';

vi.mock('../api/calm', async () => {
  const actual = await vi.importActual<typeof import('../api/calm')>(
    '../api/calm',
  );
  return {
    ...actual,
    createCard: vi.fn(),
    createClaudeCard: vi.fn(),
    createCodexCard: vi.fn(),
    createTerminalCard: vi.fn(),
  };
});

import * as api from '../api/calm';
import type { KernelCard } from '../api/wire';
import {
  addCardWithValues,
  assertRouterCreateAllowed,
  CatalogCreateNotImplemented,
  KernelMintedOnlyCreateNotAllowed,
} from '../app/router';
import { IframeEntry } from './builtins/iframe';
import { TerminalEntry } from './builtins/terminal';
import { ClaudeEntry, CodexEntry } from './builtins/codex';
import type { WaveCardData } from '../types';
import {
  __resetRegistryForTest,
  adaptKernelCard,
  addPanelEntries,
  CardInstanceProvider,
  getEntry,
  registerCard,
  renderCard,
  type CardEntry,
  useCardInstanceCtx,
  useCardLifecycle,
} from './registry';
import {
  __resetCardEntryResolverRegistryForTest,
  resolveCardById,
} from './resolver';

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
    'test-controller-missing': TestControllerMissingCardData;
    'test-controller-conflict': TestControllerConflictCardData;
    'test-controller-epoch': TestControllerEpochCardData;
    'test-controller-refresh': TestControllerRefreshCardData;
    'test-controller-dispose': TestControllerDisposeCardData;
    'test-controller-rebuild': TestControllerRebuildCardData;
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
interface TestControllerMissingCardData {
  type: 'test-controller-missing';
  id: string;
}
interface TestControllerConflictCardData {
  type: 'test-controller-conflict';
  id: string;
}
interface TestControllerEpochCardData {
  type: 'test-controller-epoch';
  id: string;
}
interface TestControllerRefreshCardData {
  type: 'test-controller-refresh';
  id: string;
}
interface TestControllerDisposeCardData {
  type: 'test-controller-dispose';
  id: string;
}
interface TestControllerRebuildCardData {
  type: 'test-controller-rebuild';
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
  __resetCardEntryResolverRegistryForTest();
  vi.clearAllMocks();
});

afterEach(() => {
  vi.restoreAllMocks();
});

function queryClientStub(): Parameters<typeof addCardWithValues>[0] {
  return {
    invalidateQueries: vi.fn().mockResolvedValue(undefined),
  } as unknown as Parameters<typeof addCardWithValues>[0];
}

describe('card instance slots', () => {
  it('resolves lazy initializers once across repeated slot reads', () => {
    const init = vi.fn(() => 42);

    function Probe({ label }: { label: string }) {
      const ctx = useCardInstanceCtx();
      const [first] = ctx.useCardSlot<number>('k', init);
      const [second] = ctx.useCardSlot<number>('k', init);
      return (
        <div data-testid="slot">
          {label}:{first}/{second}
        </div>
      );
    }

    const { rerender } = render(
      <CardInstanceProvider cardId="card_slot" deletable>
        <Probe label="a" />
      </CardInstanceProvider>,
    );

    expect(screen.getByTestId('slot')).toHaveTextContent('a:42/42');

    rerender(
      <CardInstanceProvider cardId="card_slot" deletable>
        <Probe label="b" />
      </CardInstanceProvider>,
    );

    expect(screen.getByTestId('slot')).toHaveTextContent('b:42/42');
    expect(init).toHaveBeenCalledTimes(1);
  });

  it('warns in DEV when later reads use an inconsistent initial value', () => {
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

    function Probe({ initial }: { initial: number }) {
      const [value] = useCardInstanceCtx().useCardSlot<number>('k', initial);
      return <div data-testid="slot">{value}</div>;
    }

    const { rerender } = render(
      <CardInstanceProvider cardId="card_warn" deletable>
        <Probe initial={1} />
      </CardInstanceProvider>,
    );

    rerender(
      <CardInstanceProvider cardId="card_warn" deletable>
        <Probe initial={2} />
      </CardInstanceProvider>,
    );

    expect(screen.getByTestId('slot')).toHaveTextContent('1');
    if (import.meta.env.DEV) {
      expect(warnSpy).toHaveBeenCalledWith(
        expect.stringContaining(
          '[useCardSlot] inconsistent initial for cardId=card_warn key="k"',
        ),
      );
      expect(String(warnSpy.mock.calls[0]![0])).toContain(
        'first read used 1',
      );
      expect(String(warnSpy.mock.calls[0]![0])).toContain('this read uses 2');
    } else {
      expect(warnSpy).not.toHaveBeenCalled();
    }
  });
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

  it('requires controller refresh backing to provide a controller', () => {
    expect(() =>
      registerCard(
        entry<TestControllerMissingCardData>({
          type: 'test-controller-missing',
          refreshBacking: 'controller',
        }),
      ),
    ).toThrow('RefreshBackingMissingController(test-controller-missing)');
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

describe('card controller lifecycle contract', () => {
  it('throws on epoch refresh backing with controller onRefresh', () => {
    registerCard(
      entry<TestControllerConflictCardData>({
        type: 'test-controller-conflict',
        refreshBacking: 'epoch',
        createController: () => ({ onRefresh: () => {} }),
      }),
    );

    expect(() =>
      render(
        <>
          {renderCard({
            type: 'test-controller-conflict',
            id: 'card_conflict',
          })}
        </>,
      ),
    ).toThrow(
      'RefreshBackingConflict(test-controller-conflict): refreshBacking=epoch forbids controller.onRefresh; use refreshBacking=controller or remove onRefresh.',
    );
  });

  it('mounts epoch refresh backing controllers without onRefresh', () => {
    registerCard(
      entry<TestControllerEpochCardData>({
        type: 'test-controller-epoch',
        refreshBacking: 'epoch',
        createController: () => ({ onVisibleChange: () => {} }),
      }),
    );

    expect(() =>
      render(
        <>
          {renderCard({
            type: 'test-controller-epoch',
            id: 'card_epoch',
          })}
        </>,
      ),
    ).not.toThrow();
  });

  it('exposes the lifecycle writer only through the host resolver', async () => {
    let childLifecycle: ReturnType<typeof useCardLifecycle> | null = null;

    function LifecycleProbe() {
      childLifecycle = useCardLifecycle();
      return <div>probe</div>;
    }

    registerCard(
      entry<TestControllerEpochCardData>({
        type: 'test-controller-epoch',
        Component: LifecycleProbe,
      }),
    );

    render(
      <>
        {renderCard({
          type: 'test-controller-epoch',
          id: 'card_lifecycle',
        })}
      </>,
    );

    await waitFor(() =>
      expect(resolveCardById('card_lifecycle')?.writer).toBeDefined(),
    );

    const resolved = resolveCardById('card_lifecycle');
    expect(resolved?.writer.setVisible).toEqual(expect.any(Function));
    expect(resolved?.writer.setFocused).toEqual(expect.any(Function));
    expect(childLifecycle).not.toBeNull();
    if (!childLifecycle) throw new Error('LifecycleProbe did not render');
    expect(Object.isFrozen(childLifecycle)).toBe(true);
    expect('setVisible' in childLifecycle).toBe(false);
    expect('setFocused' in childLifecycle).toBe(false);
    expect(childLifecycle).not.toBe(resolved?.writer);
  });

  it('routes emitted refresh commands through the lifecycle epoch', () => {
    const onRefresh = vi.fn();
    let lifecycleEpoch = -1;

    function RefreshProbe() {
      const ctx = useCardInstanceCtx();
      const lifecycle = useCardLifecycle();
      lifecycleEpoch = lifecycle.getSnapshot().refreshEpoch;
      return (
        <button
          type="button"
          onClick={() => ctx.emit({ type: 'refresh' })}
        >
          refresh
        </button>
      );
    }

    registerCard(
      entry<TestControllerRefreshCardData>({
        type: 'test-controller-refresh',
        Component: RefreshProbe,
        refreshBacking: 'controller',
        createController: ({ lifecycle }) => ({
          onRefresh: () => {
            lifecycleEpoch = lifecycle.getSnapshot().refreshEpoch;
            onRefresh();
          },
        }),
      }),
    );

    render(
      <>
        {renderCard({
          type: 'test-controller-refresh',
          id: 'card_refresh',
        })}
      </>,
    );

    expect(lifecycleEpoch).toBe(0);
    expect(onRefresh).not.toHaveBeenCalled();

    act(() => {
      fireEvent.click(screen.getByRole('button', { name: 'refresh' }));
    });
    expect(lifecycleEpoch).toBe(1);
    expect(onRefresh).toHaveBeenCalledTimes(1);

    act(() => {
      fireEvent.click(screen.getByRole('button', { name: 'refresh' }));
    });
    expect(lifecycleEpoch).toBe(2);
    expect(onRefresh).toHaveBeenCalledTimes(2);
  });

  it('disposes controllers on unmount exactly once', () => {
    const dispose = vi.fn();

    registerCard(
      entry<TestControllerDisposeCardData>({
        type: 'test-controller-dispose',
        createController: () => ({ dispose }),
      }),
    );

    const { unmount } = render(
      <>
        {renderCard({
          type: 'test-controller-dispose',
          id: 'card_dispose',
        })}
      </>,
    );

    expect(dispose).not.toHaveBeenCalled();

    unmount();

    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it('rebuilds controllers when cardId changes', () => {
    const firstDispose = vi.fn();
    const secondDispose = vi.fn();
    const createController = vi
      .fn()
      .mockReturnValueOnce({ dispose: firstDispose })
      .mockReturnValueOnce({ dispose: secondDispose });

    registerCard(
      entry<TestControllerRebuildCardData>({
        type: 'test-controller-rebuild',
        createController,
      }),
    );

    const { rerender } = render(
      <>
        {renderCard({
          type: 'test-controller-rebuild',
          id: 'card_before',
        })}
      </>,
    );

    expect(createController).toHaveBeenCalledTimes(1);
    expect(createController).toHaveBeenLastCalledWith(
      expect.objectContaining({
        card: expect.objectContaining({ id: 'card_before' }),
        instance: expect.objectContaining({ cardId: 'card_before' }),
      }),
    );
    expect(firstDispose).not.toHaveBeenCalled();

    rerender(
      <>
        {renderCard({
          type: 'test-controller-rebuild',
          id: 'card_after',
        })}
      </>,
    );

    expect(firstDispose).toHaveBeenCalledTimes(1);
    expect(createController).toHaveBeenCalledTimes(2);
    expect(createController).toHaveBeenLastCalledWith(
      expect.objectContaining({
        card: expect.objectContaining({ id: 'card_after' }),
        instance: expect.objectContaining({ cardId: 'card_after' }),
      }),
    );
    expect(secondDispose).not.toHaveBeenCalled();
  });

  it('throws when useCardLifecycle is used outside CardInstanceProvider', () => {
    function Probe() {
      useCardLifecycle();
      return null;
    }

    expect(() => render(<Probe />)).toThrow(
      'useCardLifecycle outside CardInstanceProvider',
    );
  });
});

describe('router AddPanel create runtime failures', () => {
  it('swallows iframe create schema parse failures at the router edge', async () => {
    registerCard(IframeEntry);
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const qc = queryClientStub();

    await expect(
      addCardWithValues(
        qc,
        'wave_1',
        'iframe',
        { url: 'javascript:alert(1)' },
        'dark',
      ),
    ).resolves.toBeUndefined();

    expect(api.createCard).not.toHaveBeenCalled();
    expect(qc.invalidateQueries).not.toHaveBeenCalled();
    expect(warnSpy).toHaveBeenCalledWith(
      '[Calm] iframe create rejected invalid input:',
      expect.any(Error),
    );
  });

  it('swallows rejected atomic terminal creates at the router edge', async () => {
    registerCard(TerminalEntry);
    const err = new Error('offline');
    vi.mocked(api.createTerminalCard).mockRejectedValue(err);
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const qc = queryClientStub();

    await expect(
      addCardWithValues(qc, 'wave_1', 'terminal', {}, 'dark'),
    ).resolves.toBeUndefined();

    expect(api.createTerminalCard).toHaveBeenCalled();
    expect(qc.invalidateQueries).not.toHaveBeenCalled();
    expect(warnSpy).toHaveBeenCalledWith(
      '[Calm] terminal create failed:',
      err,
    );
  });

  it('keeps router create contract errors propagating', async () => {
    registerCard(
      entry<TestCatalogCardData>({
        type: 'test-catalog',
        claim: { mode: 'prefix', prefix: 'ui://' },
        create: { mode: 'catalog', catalog: 'plugin-views' },
      }),
    );
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

    await expect(
      addCardWithValues(queryClientStub(), 'wave_1', 'test-catalog', {}, 'dark'),
    ).rejects.toThrow(CatalogCreateNotImplemented);

    expect(warnSpy).not.toHaveBeenCalled();
  });
});

describe('built-in atomic create entries', () => {
  const themeRgb = {
    fg: [1, 2, 3] as [number, number, number],
    bg: [4, 5, 6] as [number, number, number],
  };

  it('submits terminal creates through the atomic terminal endpoint', async () => {
    vi.mocked(api.createTerminalCard).mockResolvedValue({
      id: 'card_terminal',
    } as Awaited<ReturnType<typeof api.createTerminalCard>>);

    expect(TerminalEntry.create?.mode).toBe('atomic');
    if (TerminalEntry.create?.mode !== 'atomic') {
      throw new Error('terminal create is not atomic');
    }

    await expect(
      TerminalEntry.create.submit('wave_1', {}, { themeRgb }),
    ).resolves.toEqual({
      cardId: 'card_terminal',
      raw: { id: 'card_terminal' },
    });
    expect(api.createTerminalCard).toHaveBeenCalledWith('wave_1', {
      theme: themeRgb,
    });
  });

  it('submits codex creates through the atomic codex endpoint', async () => {
    vi.mocked(api.createCodexCard).mockResolvedValue({
      id: 'card_codex',
    } as Awaited<ReturnType<typeof api.createCodexCard>>);

    expect(CodexEntry.create?.mode).toBe('atomic');
    if (CodexEntry.create?.mode !== 'atomic') {
      throw new Error('codex create is not atomic');
    }

    await expect(
      CodexEntry.create.submit('wave_1', { cwd: '' }, { themeRgb }),
    ).resolves.toEqual({
      cardId: 'card_codex',
      raw: { id: 'card_codex' },
    });
    expect(api.createCodexCard).toHaveBeenCalledWith('wave_1', {
      cwd: undefined,
      prompt: undefined,
      theme: themeRgb,
    });
  });

  it('submits claude creates through the atomic claude endpoint', async () => {
    vi.mocked(api.createClaudeCard).mockResolvedValue({
      id: 'card_claude',
    } as Awaited<ReturnType<typeof api.createClaudeCard>>);

    expect(ClaudeEntry.create?.mode).toBe('atomic');
    if (ClaudeEntry.create?.mode !== 'atomic') {
      throw new Error('claude create is not atomic');
    }

    await expect(
      ClaudeEntry.create.submit(
        'wave_1',
        { cwd: '/repo', prompt: 'ship it' },
        { themeRgb },
      ),
    ).resolves.toEqual({
      cardId: 'card_claude',
      raw: { id: 'card_claude' },
    });
    expect(api.createClaudeCard).toHaveBeenCalledWith('wave_1', {
      cwd: '/repo',
      prompt: 'ship it',
      theme: themeRgb,
    });
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
