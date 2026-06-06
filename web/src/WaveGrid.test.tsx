// Component-level tests for `WaveGrid` (overlay-backed layout).
//
// What we lock in here:
//
//   1. **Initial render reflects the overlay GET.** Mount with a seeded
//      `listOverlays` response and assert the rendered grid items carry
//      the stored coordinates.
//   2. **Drag end fires a single POST.** RGL's `onLayoutChange` is the
//      drag-time firehose; the rAF-coalesced setter inside WaveGrid
//      must collapse a burst into one mutation per visual frame.
//
// We mock `api/calm.ts` wholesale (same pattern as the queries tests) and
// stub `react-grid-layout` to capture the `layout` prop + expose the
// `onLayoutChange` callback. The real RGL is a heavy DOM library that
// brings nothing to a position-persistence assertion.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  act,
  fireEvent,
  render,
  waitFor,
  cleanup,
  screen,
} from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';

vi.mock('./api/calm', () => ({
  listOverlays: vi.fn(),
  upsertOverlay: vi.fn(),
}));

// react-grid-layout stub: capture the layout prop + onLayoutChange in a
// module-scoped slot so each test can read the most recent layout the
// component handed RGL, and synthetically invoke `onLayoutChange` to
// simulate a drag end.
type GridCapture = {
  layout: Array<{ i: string; x: number; y: number; w: number; h: number }>;
  onLayoutChange: ((next: GridCapture['layout']) => void) | null;
};
const grid: GridCapture = { layout: [], onLayoutChange: null };
vi.mock('react-grid-layout', () => {
  return {
    useContainerWidth: () => ({
      width: 1200,
      // `containerRef` must be an actual ref to satisfy React's typing;
      // a plain `{ current: null }` object works at runtime.
      containerRef: { current: null },
      mounted: true,
    }),
    GridLayout: (props: {
      layout: GridCapture['layout'];
      onLayoutChange: GridCapture['onLayoutChange'];
      children?: ReactNode;
    }) => {
      grid.layout = props.layout;
      grid.onLayoutChange = props.onLayoutChange;
      return <div data-testid="grid-stub">{props.children}</div>;
    },
  };
});

import * as api from './api/calm';
import { WaveGrid } from './WaveGrid';
import type { WaveCardSlot, WaveCardData } from './types';
import type { KernelOverlay } from './api/wire';
import {
  __resetRegistryForTest,
  registerCard,
  type CardEntry,
} from './cards/registry';
import { __resetCardEntryResolverRegistryForTest } from './cards/resolver';

declare module './types' {
  interface WaveCardDataMap {
    'grid-visibility-test': GridVisibilityCardData;
  }
}

interface GridVisibilityCardData {
  type: 'grid-visibility-test';
  id: string;
}

const originalIntersectionObserver = globalThis.IntersectionObserver;

class FakeIntersectionObserver {
  static instances: FakeIntersectionObserver[] = [];

  private readonly callback: IntersectionObserverCallback;
  observed = new Set<Element>();
  observe = vi.fn((target: Element) => {
    this.observed.add(target);
  });
  unobserve = vi.fn((target: Element) => {
    this.observed.delete(target);
  });
  disconnect = vi.fn(() => {
    this.observed.clear();
  });
  takeRecords = vi.fn(() => []);

  constructor(callback: IntersectionObserverCallback) {
    this.callback = callback;
    FakeIntersectionObserver.instances.push(this);
  }

  fire(entries: Array<Partial<IntersectionObserverEntry> & { target: Element }>) {
    this.callback(
      entries as IntersectionObserverEntry[],
      this as unknown as IntersectionObserver,
    );
  }
}

function card(
  id: string,
  kind: 'terminal' | 'codex' = 'terminal',
  opts: { deletable?: boolean } = {},
): WaveCardSlot {
  const data: WaveCardData =
    kind === 'codex'
      ? { type: 'codex', id }
      : { type: 'terminal', id, title: id, lines: [], terminalId: `t-${id}` };
  // Issue #229 PR A — let tests override the deletable bit so the
  // `deletable=false → no close X` invariant can be exercised. Default
  // (`undefined`) means "user-deletable" per WaveGrid's
  // `card.deletable !== false` check.
  return { kind: 'card', card: data, deletable: opts.deletable };
}

function visibilityCard(id: string): WaveCardSlot {
  return {
    kind: 'card',
    card: { type: 'grid-visibility-test', id },
  };
}

function registerVisibilityEntry({
  onVisibleChange = vi.fn(),
  onFocusChange = vi.fn(),
  onUnmount = vi.fn(),
}: {
  onVisibleChange?: (visible: boolean) => void;
  onFocusChange?: (focused: boolean) => void;
  onUnmount?: () => void;
}) {
  function VisibilityCard() {
    useEffect(() => () => onUnmount());
    return (
      <div data-testid="grid-visibility-card">
        <button type="button" aria-label="Head focus target">
          head
        </button>
        <button type="button" aria-label="Body focus target">
          body
        </button>
      </div>
    );
  }

  registerCard({
    type: 'grid-visibility-test',
    Component: VisibilityCard,
    defaultSize: { w: 4, h: 3, minW: 2, minH: 2 },
    title: () => 'visibility',
    accessibleName: () => 'Visibility test card',
    create: { mode: 'kernel-minted-only' },
    createController: () => ({
      onVisibleChange,
      onFocusChange,
    }),
  } as CardEntry<GridVisibilityCardData>);
}

function layoutOverlay(
  positions: Record<string, { x: number; y: number; w: number; h: number }>,
): KernelOverlay {
  return {
    id: 'ov-1',
    plugin_id: 'kernel',
    entity_kind: 'view',
    entity_id: 'w1',
    kind: 'layout',
    payload: { positions } as unknown,
    updated_at: 0,
  };
}

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0, staleTime: 0 },
      mutations: { retry: false },
    },
  });
}

function Wrapper({
  client,
  children,
}: {
  client: QueryClient;
  children: ReactNode;
}) {
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

beforeEach(() => {
  vi.clearAllMocks();
  __resetRegistryForTest();
  __resetCardEntryResolverRegistryForTest();
  FakeIntersectionObserver.instances = [];
  globalThis.IntersectionObserver =
    FakeIntersectionObserver as unknown as typeof IntersectionObserver;
  grid.layout = [];
  grid.onLayoutChange = null;
  cleanup();
});

afterEach(() => {
  if (originalIntersectionObserver) {
    globalThis.IntersectionObserver = originalIntersectionObserver;
  } else {
    const mutableGlobal = globalThis as {
      IntersectionObserver?: typeof IntersectionObserver;
    };
    delete mutableGlobal.IntersectionObserver;
  }
  __resetCardEntryResolverRegistryForTest();
});

describe('WaveGrid — overlay-backed layout', () => {
  it('renders with positions from the overlay GET', async () => {
    const stored = {
      'card-a': { x: 0, y: 0, w: 4, h: 3 },
      'card-b': { x: 4, y: 0, w: 4, h: 3 },
    };
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([
      layoutOverlay(stored),
    ]);

    const client = makeClient();
    render(
      <Wrapper client={client}>
        <WaveGrid
          waveId="w1"
          cards={[card('card-a'), card('card-b')]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    // First render returns the default ({ positions: {} }) — both cards
    // get auto-packed. After the GET resolves the component re-reconciles
    // against the stored positions; that's what we assert here.
    await waitFor(() => {
      const a = grid.layout.find((l) => l.i === 'card-a')!;
      const b = grid.layout.find((l) => l.i === 'card-b')!;
      expect(a.x).toBe(0);
      expect(a.w).toBe(4);
      expect(b.x).toBe(4);
      expect(b.w).toBe(4);
    });
  });

  // Issue #229 PR A — kernel-owned card slots (`deletable: false` on
  // the slot, propagated from the kernel `Card.deletable` bit) render
  // without the close X. The card head's close button has a stable
  // `aria-label="Remove panel"` in WaveGrid contexts (set on every
  // built-in card's `<CardHead onClose=… closeAriaLabel="Remove panel"
  // />` callsite). Querying by that label is the cleanest contract —
  // `closable: false` means zero matches; `closable: true` (the
  // default) means one per card.
  it('hides close X when slot deletable === false', async () => {
    // Use a `kind: 'unknown'` slot to keep the DOM minimal — UnknownCard
    // renders a single `<CardHead>` with no theme / overlay
    // dependencies. The same WaveGrid wiring decides
    // `closable = slot.deletable !== false` for both `'card'` and
    // `'unknown'` slot shapes, so testing the X-suppression contract
    // on the simpler slot exercises the same branch.
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    const client = makeClient();
    const undeletable: WaveCardSlot = {
      kind: 'unknown',
      id: 'spec-card',
      kernelKind: 'codex',
      deletable: false,
    };
    const { container } = render(
      <Wrapper client={client}>
        <WaveGrid
          waveId="w1"
          cards={[undeletable]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );
    // The close affordance lives on `<CardHead>`'s `.card-grid-close`
    // button. With the slot marked undeletable, WaveGrid passes
    // `onClose: undefined` and CardHead skips rendering the button.
    const closeButtons = container.querySelectorAll('button.card-grid-close');
    expect(closeButtons.length).toBe(0);
  });

  it('renders close X when slot deletable === true', async () => {
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    const client = makeClient();
    const deletable: WaveCardSlot = {
      kind: 'unknown',
      id: 'plain-card',
      kernelKind: 'codex',
      deletable: true,
    };
    const { container } = render(
      <Wrapper client={client}>
        <WaveGrid
          waveId="w1"
          cards={[deletable]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );
    // UnknownCard renders one `<CardHead>` → one close button.
    const closeButtons = container.querySelectorAll('button.card-grid-close');
    expect(closeButtons.length).toBe(1);
  });

  it('treats omitted slot.deletable as user-deletable (legacy wire payloads)', async () => {
    // Belt-and-suspenders for event-log replays + older `KernelCard`
    // shapes that don't carry the field. The slot constructor in
    // `app/router.tsx` propagates `k.deletable` straight through; when
    // that's `undefined`, WaveGrid must keep the close button visible.
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    const client = makeClient();
    // Constructed without `deletable` field → slot.deletable is undefined.
    const legacy: WaveCardSlot = {
      kind: 'unknown',
      id: 'legacy',
      kernelKind: 'codex',
    };
    const { container } = render(
      <Wrapper client={client}>
        <WaveGrid
          waveId="w1"
          cards={[legacy]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );
    const closeButtons = container.querySelectorAll('button.card-grid-close');
    expect(closeButtons.length).toBe(1);
  });

  it('routes shell intersection changes to the card controller without unmounting', async () => {
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    const onVisibleChange = vi.fn();
    const onUnmount = vi.fn();
    registerVisibilityEntry({ onVisibleChange, onUnmount });

    render(
      <Wrapper client={makeClient()}>
        <WaveGrid
          waveId="w1"
          cards={[visibilityCard('grid_visibility')]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const shell = screen
      .getByTestId('grid-visibility-card')
      .closest<HTMLElement>('[data-card-id]');
    expect(shell).not.toBeNull();
    await waitFor(() =>
      expect(FakeIntersectionObserver.instances[0]?.observe).toHaveBeenCalledWith(
        shell,
      ),
    );

    act(() => {
      FakeIntersectionObserver.instances[0]!.fire([
        { target: shell!, isIntersecting: false },
      ]);
    });
    expect(onVisibleChange).toHaveBeenCalledWith(false);
    expect(onUnmount).not.toHaveBeenCalled();
    expect(screen.getByTestId('grid-visibility-card')).toBeInTheDocument();

    act(() => {
      FakeIntersectionObserver.instances[0]!.fire([
        { target: shell!, isIntersecting: true },
      ]);
    });
    expect(onVisibleChange).toHaveBeenLastCalledWith(true);
  });

  it('routes focus into, within, and out of a card shell', async () => {
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    const onFocusChange = vi.fn();
    registerVisibilityEntry({ onFocusChange });
    const outside = document.createElement('button');
    document.body.append(outside);

    render(
      <Wrapper client={makeClient()}>
        <WaveGrid
          waveId="w1"
          cards={[visibilityCard('grid_focus')]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );

    const head = screen.getByRole('button', { name: 'Head focus target' });
    const body = screen.getByRole('button', { name: 'Body focus target' });
    await waitFor(() => expect(FakeIntersectionObserver.instances[0]).toBeDefined());

    fireEvent.focusIn(head);
    await waitFor(() => expect(onFocusChange).toHaveBeenCalledWith(true));

    fireEvent.focusOut(head, { relatedTarget: body });
    fireEvent.focusIn(body, { relatedTarget: head });
    expect(onFocusChange).toHaveBeenCalledTimes(1);

    fireEvent.focusOut(body, { relatedTarget: outside });
    await waitFor(() => expect(onFocusChange).toHaveBeenLastCalledWith(false));
  });

  it('drag end fires a single POST with the new positions', async () => {
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    (api.upsertOverlay as ReturnType<typeof vi.fn>).mockResolvedValue(
      layoutOverlay({}),
    );

    const client = makeClient();
    render(
      <Wrapper client={client}>
        <WaveGrid
          waveId="w1"
          cards={[card('card-a')]}
          onRemoveCard={() => {}}
        />
      </Wrapper>,
    );
    await waitFor(() => expect(grid.onLayoutChange).not.toBeNull());

    // Simulate a drag-event firehose: RGL fires `onLayoutChange` once
    // per pointer-move during a drag. We fire several in quick
    // succession; the rAF-coalesced setter inside WaveGrid must
    // collapse them into a single POST.
    const finalLayout = [
      { i: 'card-a', x: 3, y: 2, w: 4, h: 3 },
    ];
    act(() => {
      grid.onLayoutChange!([{ i: 'card-a', x: 0, y: 0, w: 4, h: 3 }]);
      grid.onLayoutChange!([{ i: 'card-a', x: 1, y: 1, w: 4, h: 3 }]);
      grid.onLayoutChange!([{ i: 'card-a', x: 2, y: 1, w: 4, h: 3 }]);
      grid.onLayoutChange!(finalLayout);
    });

    // The rAF wakeup writes once with the latched (last) layout.
    await waitFor(() => expect(api.upsertOverlay).toHaveBeenCalledTimes(1));
    expect(api.upsertOverlay).toHaveBeenCalledWith({
      plugin_id: 'kernel',
      entity_kind: 'view',
      entity_id: 'w1',
      kind: 'layout',
      payload: {
        // Tier A: kernel-owned overlay payloads carry a per-kind
        // `schemaVersion` on every write. The validator treats absent
        // as v1, so older clients still work; new writes stamp it.
        schemaVersion: 1,
        positions: {
          'card-a': { x: 3, y: 2, w: 4, h: 3 },
        },
      },
    });
  });

});
