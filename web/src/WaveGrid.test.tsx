// Component-level tests for `WaveGrid` after the Scope E migration.
//
// What we lock in here:
//
//   1. **Initial render reflects the overlay GET.** Mount with a seeded
//      `listOverlays` response and assert the rendered grid items carry
//      the stored coordinates.
//   2. **Drag end fires a single POST.** RGL's `onLayoutChange` is the
//      drag-time firehose; the rAF-coalesced setter inside WaveGrid
//      must collapse a burst into one mutation per visual frame.
//   3. **localStorage → overlay migration.** When `listOverlays` returns
//      empty but `localStorage['calm:layout:<waveId>']` carries legacy
//      positions, the first mount POSTs an overlay carrying them and
//      removes the localStorage key.
//
// We mock `api/calm.ts` wholesale (same pattern as the queries tests) and
// stub `react-grid-layout` to capture the `layout` prop + expose the
// `onLayoutChange` callback. The real RGL is a heavy DOM library that
// brings nothing to a position-persistence assertion.

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, render, waitFor, cleanup } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

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

function card(id: string, kind: 'terminal' | 'codex' = 'terminal'): WaveCardSlot {
  const data: WaveCardData =
    kind === 'codex'
      ? { type: 'codex', id }
      : { type: 'terminal', id, title: id, lines: [], terminalId: `t-${id}` };
  return { kind: 'card', card: data };
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
  grid.layout = [];
  grid.onLayoutChange = null;
  cleanup();
  localStorage.clear();
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
        positions: {
          'card-a': { x: 3, y: 2, w: 4, h: 3 },
        },
      },
    });
  });

  it('migrates legacy localStorage layout on first mount', async () => {
    // No overlay row yet → migration path triggers.
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    (api.upsertOverlay as ReturnType<typeof vi.fn>).mockResolvedValue(
      layoutOverlay({ 'card-a': { x: 1, y: 1, w: 4, h: 3 } }),
    );
    localStorage.setItem(
      'calm:layout:w1',
      JSON.stringify({ 'card-a': { x: 1, y: 1, w: 4, h: 3 } }),
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

    // The migration helper POSTs the parsed positions and then removes
    // the localStorage key. We assert both.
    await waitFor(() => expect(api.upsertOverlay).toHaveBeenCalled());
    expect(api.upsertOverlay).toHaveBeenCalledWith({
      plugin_id: 'kernel',
      entity_kind: 'view',
      entity_id: 'w1',
      kind: 'layout',
      payload: { positions: { 'card-a': { x: 1, y: 1, w: 4, h: 3 } } },
    });
    await waitFor(() =>
      expect(localStorage.getItem('calm:layout:w1')).toBeNull(),
    );
  });

  it('does not migrate when overlay row already exists', async () => {
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([
      layoutOverlay({ 'card-a': { x: 0, y: 0, w: 4, h: 3 } }),
    ]);
    localStorage.setItem(
      'calm:layout:w1',
      JSON.stringify({ 'card-a': { x: 9, y: 9, w: 1, h: 1 } }),
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

    // Wait for the GET to resolve so the migration effect has a chance
    // to run. It should NOT call upsertOverlay — the overlay row is
    // already authoritative.
    await waitFor(() => expect(api.listOverlays).toHaveBeenCalled());
    // A small grace tick for the effect to settle.
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(api.upsertOverlay).not.toHaveBeenCalled();
  });
});
