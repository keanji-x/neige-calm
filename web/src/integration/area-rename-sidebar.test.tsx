// Issue #288 — area rename → sidebar entry must reflect the new name.
//
// The bug repros against `make dev` / production: after renaming an area
// via the area-page header inline rename, the sidebar entry still shows
// the OLD name until a hard refresh. The kernel write succeeds,
// `area.updated` fires on the bus, and `GET /api/areas` returns the new
// name — but the sidebar doesn't repaint.
//
// This integration test wires the production seam end-to-end at the
// React-Query layer:
//
//   * `useUpdateAreaMutation` (the mutation `Area.tsx` triggers on Enter
//     / blur via `onRenameArea`).
//   * A `<Sidebar>` rendered alongside, reading the SAME QueryClient via
//     `useAreasQuery` — the exact pattern `CalmApp` uses in production.
//
// The optimistic-update path in `useUpdateAreaMutation` writes the
// renamed area into `['areas']` synchronously inside `onMutate`; the
// sidebar observer subscribed to that key must repaint with the new
// name on the next render commit. Asserting through the rendered DOM
// (not just `getQueryData`) catches a future regression where the
// optimistic write hits the cache but a memo / selector / mapper traps
// the stale value on the render path.
//
// Why not e2e: the hermetic Playwright env doesn't repro the bug
// (PR #291 confirmed this). The user's repro is against a production
// bundle. A vitest-level integration test pins the exact React-Query +
// component-tree contract the production code depends on — close
// enough to the production wiring to catch a regression in any of:
//   - the mutation's optimistic write
//   - the eventBridge invalidation
//   - the Sidebar's data flow from `useAreasQuery` → area row.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Hoisted REST-client mock — every query/mutation here ultimately calls
// one of these. We control the resolutions per-test.
vi.mock('../api/calm', () => ({
  listAreas: vi.fn(),
  tracksInArea: vi.fn(),
  getTrackDetail: vi.fn(),
  createArea: vi.fn(),
  updateArea: vi.fn(),
  deleteArea: vi.fn(),
  createTrack: vi.fn(),
  updateTrack: vi.fn(),
  deleteTrack: vi.fn(),
  createCard: vi.fn(),
  updateCard: vi.fn(),
  deleteCard: vi.fn(),
  listAllOverlays: vi.fn(),
  getSettings: vi.fn(),
  putSettings: vi.fn(),
}));

import * as api from '../api/calm';
import { Sidebar } from '../shared/components/Sidebar';
import { AreaPage } from '../pages/Area';
import { useAreasQuery, useUpdateAreaMutation } from '../api/queries';
import { adaptArea } from '../api/adapt';
import type { KernelArea } from '../api/wire';
import type { Area, Route } from '../types';
import { SessionContext } from '../app/SessionProvider';

// Sidebar's UserMenu calls `useSession()`. Tests don't stand up the real
// SessionProvider (which would probe `/api/auth/whoami`); instead they
// wrap renders in a stub provider that carries a minimal whoami payload.
const STUB_SESSION = {
  userId: 'u-test',
  displayName: 'Test User',
  role: 'owner',
  sessionId: 's-test',
};

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: Infinity, staleTime: Infinity },
      mutations: { retry: false },
    },
  });
}

function wrap(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={client}>
        <SessionContext.Provider value={STUB_SESSION}>
          {children}
        </SessionContext.Provider>
      </QueryClientProvider>
    );
  };
}

function makeKernelArea(id: string, name: string): KernelArea {
  return {
    id,
    name,
    color: '#5a9',
    sort: 0,
    kind: 'user',
    created_at: 1,
    updated_at: 2,
  };
}

// ---- The integration scenario -----------------------------------------
//
// `SidebarHost` matches the data flow in `CalmApp.tsx`:
//   - calls `useAreasQuery`
//   - filters to `kind === 'user'`
//   - maps via `adaptArea`
//   - hands the result to <Sidebar />
//
// `Renamer` exposes a `triggerRename` helper that mirrors what
// `Area.tsx`'s `EditableTitle.save()` does on Enter / blur — it calls
// `useUpdateAreaMutation().mutateAsync({ id, body: { name } })`.

function SidebarHost({ route }: { route: Route }) {
  const areasQ = useAreasQuery();
  const kernelAreas = (areasQ.data ?? []).filter((c) => c.kind === 'user');
  const areas: Area[] = kernelAreas.map(adaptArea);
  // The "tracks" prop is required by Sidebar. Pass an empty array — the
  // rename path is independent of track state, and the area-nav button
  // text (the surface this test asserts) renders from `areas` alone.
  return <Sidebar areas={areas} tracks={[]} route={route} onGo={() => {}} />;
}

function Renamer({
  onReady,
}: {
  onReady: (rename: (id: string, name: string) => Promise<void>) => void;
}) {
  const update = useUpdateAreaMutation();
  // Stash the rename helper on first render so the test driver can
  // trigger the mutation from outside the React tree.
  onReady(async (id, name) => {
    await update.mutateAsync({ id, body: { name } });
  });
  return null;
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
});

function querySidebarAreaNavButton(name: RegExp | string): HTMLButtonElement | null {
  const sidebar = screen.getByRole('navigation', { name: 'Areas' });
  const buttons = Array.from(
    sidebar.querySelectorAll<HTMLButtonElement>('button.area-nav'),
  );
  return buttons.find((button) => {
    const text = button.textContent ?? '';
    return typeof name === 'string' ? text === name : name.test(text);
  }) ?? null;
}

function getSidebarAreaNavButton(name: RegExp | string): HTMLButtonElement {
  const button = querySidebarAreaNavButton(name);
  if (!button) throw new Error(`Could not find sidebar area-nav button: ${name}`);
  return button;
}

describe('Issue #288 — area rename propagates to sidebar', () => {
  it('optimistically renames the sidebar area-nav entry mid-flight', async () => {
    const client = makeClient();
    const seed = [
      makeKernelArea('c1', 'OtherA'),
      makeKernelArea('c2', 'OriginalName'),
      makeKernelArea('c3', 'OtherB'),
    ];
    (api.listAreas as ReturnType<typeof vi.fn>).mockResolvedValue(seed);

    // Hold the PATCH in flight so we can read the optimistic state
    // before the server's response would land. This mirrors the
    // production timing window: the user types, blurs, the mutation
    // fires — and they look at the sidebar before the round-trip
    // completes. The optimistic update IS the immediate-feedback
    // contract; if it doesn't reach the sidebar's render, the user
    // sees stale state.
    let resolveUpdate: ((area: KernelArea) => void) | null = null;
    const pendingUpdate = new Promise<KernelArea>((resolve) => {
      resolveUpdate = resolve;
    });
    (api.updateArea as ReturnType<typeof vi.fn>).mockReturnValue(pendingUpdate);

    let triggerRename: ((id: string, name: string) => Promise<void>) | null = null;
    const wrapper = wrap(client);
    render(
      <>
        <SidebarHost route={{ name: 'today' }} />
        <Renamer onReady={(fn) => { triggerRename = fn; }} />
      </>,
      { wrapper },
    );

    // Wait for the initial area list to land in the sidebar.
    await waitFor(() => {
      expect(getSidebarAreaNavButton(/OriginalName/)).toBeInTheDocument();
    });
    expect(triggerRename).not.toBeNull();

    // Trigger the rename mutation — same call shape as Area.tsx's
    // EditableTitle `save()` ultimately drives.
    await act(async () => {
      await Promise.resolve();
      // The mutate runs synchronously through onMutate (optimistic
      // write to ['areas']). We don't await the returned promise —
      // we want to inspect the cache + DOM mid-flight, before the
      // pendingUpdate Promise resolves.
      void triggerRename!('c2', 'NewName');
      // Flush microtasks so React has a chance to commit the
      // optimistic-update-triggered re-render.
      await Promise.resolve();
      await Promise.resolve();
    });

    // The sidebar entry must reflect the new name immediately — the
    // optimistic write to ['areas'] is the contract that makes the
    // UI feel instant. If a future regression breaks the optimistic
    // path (e.g. the mutation's `onMutate` stops writing to ['areas'],
    // a memo on the render path traps the stale value, or the Sidebar
    // stops re-subscribing on cache change), this assertion fires.
    await waitFor(() => {
      expect(
        getSidebarAreaNavButton(/NewName/),
        'sidebar must show the new area name after rename mid-flight',
      ).toBeInTheDocument();
    });
    expect(
      querySidebarAreaNavButton(/OriginalName/),
      'old area name must disappear from sidebar after rename mid-flight',
    ).toBeNull();

    // Let the mutation settle so the test exits cleanly.
    await act(async () => {
      resolveUpdate!({ ...seed[1], name: 'NewName', updated_at: 99 });
      await Promise.resolve();
      await Promise.resolve();
    });
  });

  it('AreaPage EditableTitle blur rename → sidebar shows the new name', async () => {
    // End-to-end-ish: render BOTH the AreaPage (with its real
    // EditableTitle) and the Sidebar (with its area-nav button) under
    // a single QueryClient. Drive the rename through the visible UI —
    // click the title, fill the input, blur — exactly the path the
    // user reported the bug against. Assert the sidebar repaints with
    // the new name without forcing any refresh.
    const client = makeClient();
    const seed = [
      makeKernelArea('c1', 'OtherArea'),
      makeKernelArea('c2', 'OriginalAtlas'),
    ];
    (api.listAreas as ReturnType<typeof vi.fn>).mockResolvedValue(seed);
    // PATCH resolves with the renamed payload — onSettled invalidates
    // ['areas'] which will refetch and pick up the next listAreas
    // resolution. We sequence two listAreas resolutions so the
    // post-settle refetch lands the server-confirmed shape.
    const renamed = { ...seed[1], name: 'NewAtlas', updated_at: 99 };
    (api.listAreas as ReturnType<typeof vi.fn>).mockResolvedValueOnce(seed);
    (api.listAreas as ReturnType<typeof vi.fn>).mockResolvedValue([
      seed[0],
      renamed,
    ]);
    (api.updateArea as ReturnType<typeof vi.fn>).mockResolvedValue(renamed);

    function App() {
      const areasQ = useAreasQuery();
      const update = useUpdateAreaMutation();
      const kernelAreas = (areasQ.data ?? []).filter((c) => c.kind === 'user');
      const areas: Area[] = kernelAreas.map(adaptArea);
      const targetArea = areas.find((c) => c.id === 'c2');
      return (
        <>
          <SidebarHost route={{ name: 'area', areaId: 'c2' }} />
          {targetArea && (
            <AreaPage
              area={targetArea}
              tracks={[]}
              onGo={() => {}}
              onRenameArea={async (id, name) => {
                await update.mutateAsync({ id, body: { name } });
              }}
            />
          )}
        </>
      );
    }

    const wrapper = wrap(client);
    render(<App />, { wrapper });

    // Initial paint: both the sidebar and the page header carry the
    // old name. Use `findAllByRole` so the assertion waits for the
    // mounted-after-fetch render.
    await waitFor(() => {
      const matches = screen.getAllByRole('button', { name: /OriginalAtlas/ });
      // Sidebar area-nav button + AreaPage header rename button +
      // DeleteButton's confirm trigger ("Delete area \"OriginalAtlas\"").
      expect(matches.length).toBeGreaterThanOrEqual(2);
    });

    // Drive the rename via the EditableTitle in the page header.
    const user = userEvent.setup();
    const titleBtn = screen.getByRole('button', {
      name: 'OriginalAtlas',
      description: 'Rename area name',
    });
    await user.click(titleBtn);
    const input = await screen.findByRole('textbox', { name: 'Area name' });
    fireEvent.change(input, { target: { value: 'NewAtlas' } });
    // Blur via Tab to mirror the EditableTitle's `onBlur={save}` path
    // — same code path Enter takes.
    fireEvent.blur(input);

    // The sidebar entry must reflect the new name. The optimistic
    // write inside useUpdateAreaMutation.onMutate writes the renamed
    // area into ['areas'] synchronously; the SidebarHost's
    // useAreasQuery observer fires and the area-nav button repaints.
    // If a future regression breaks any link on that chain, this
    // assertion fires.
    await waitFor(() => {
      // Scope to the sidebar's area-nav button (Sidebar renders a
      // <nav aria-label="Areas">; area-nav buttons live inside).
      const sidebar = screen.getByRole('navigation', { name: 'Areas' });
      expect(
        sidebar.querySelector('button.area-nav') !== null,
        'sidebar must have area-nav buttons rendered',
      ).toBe(true);
      const newBtn = Array.from(sidebar.querySelectorAll('button.area-nav')).find((b) =>
        b.textContent?.includes('NewAtlas'),
      );
      expect(newBtn, 'sidebar area-nav must show new area name').toBeDefined();
      const oldBtn = Array.from(sidebar.querySelectorAll('button.area-nav')).find((b) =>
        b.textContent?.includes('OriginalAtlas'),
      );
      expect(oldBtn, 'sidebar area-nav must no longer show old area name').toBeUndefined();
    });
  });

  it('WS area.updated event repaints the Sidebar without depending on a refetch', async () => {
    // Issue #288 root-fix regression guard. This pins the exact path
    // the user reported the bug against:
    //
    //   1. Sidebar reads ['areas'] via useAreasQuery.
    //   2. A `area.updated` WS event arrives carrying the renamed area
    //      (kernel write already succeeded; server-emitted event).
    //   3. The Sidebar's area-nav button MUST repaint with the new
    //      name. The new write-through in eventBridge applies the
    //      payload directly to the cache, so observers see the new
    //      data on the next render — no refetch round-trip required.
    //
    // We mock listAreas to RESOLVE-AND-NEVER-REPLAY so the test
    // exposes the write-through path: if the refetch never lands,
    // the Sidebar must STILL show the new name (from the WS
    // payload-applied write).
    const client = makeClient();
    const seed = [
      makeKernelArea('c1', 'OtherArea'),
      makeKernelArea('c2', 'StaleAtlas'),
    ];
    // Initial fetch resolves. Second fetch (post-invalidate refetch)
    // returns a Promise that NEVER resolves — proving the sidebar
    // updates from the write-through, not from a refetch.
    (api.listAreas as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(seed)
      .mockReturnValue(new Promise(() => {}));

    // Render the SidebarHost AND the EventBridge under the same
    // client. The EventBridge dispatcher fires the write-through on
    // area.updated.
    const { EventBridge } = await import('../app/eventBridge');
    const { sharedEventStream } = await import('../api/events');
    // Build a tiny local fake stream so we can drive emits. We
    // mocked `../api/calm` at the module level above; the events
    // module is not mocked, so we patch sharedEventStream's
    // singleton via the same vi.mock contract.
    const fake = {
      listeners: new Set<(ev: unknown, meta: unknown) => void>(),
      subscribe: vi.fn(),
      setSyncEventVersion: vi.fn(),
      start: vi.fn(),
      on(fn: (ev: unknown, meta: unknown) => void) {
        this.listeners.add(fn);
        return () => {
          this.listeners.delete(fn);
        };
      },
      onReplayComplete() {
        return () => {};
      },
      onSnapshotRequired() {
        return () => {};
      },
      emit(ev: unknown, meta: unknown = { id: 1, eventVersion: 1 }) {
        for (const fn of this.listeners) fn(ev, meta);
      },
    };
    // Replace the singleton's exposed surface for the duration of
    // this test — sharedEventStream returns the live singleton, so
    // monkey-patch the methods the bridge calls.
    const stream = sharedEventStream();
    const orig = {
      subscribe: stream.subscribe.bind(stream),
      setSyncEventVersion: stream.setSyncEventVersion.bind(stream),
      start: stream.start.bind(stream),
      on: stream.on.bind(stream),
      onReplayComplete: stream.onReplayComplete?.bind(stream),
      onSnapshotRequired: stream.onSnapshotRequired?.bind(stream),
    };
    Object.assign(stream, {
      subscribe: fake.subscribe.bind(fake),
      setSyncEventVersion: fake.setSyncEventVersion.bind(fake),
      start: fake.start.bind(fake),
      on: fake.on.bind(fake),
      onReplayComplete: fake.onReplayComplete.bind(fake),
      onSnapshotRequired: fake.onSnapshotRequired.bind(fake),
    });

    try {
      const wrapper = wrap(client);
      render(
        <>
          <EventBridge syncEventVersion={1} />
          <SidebarHost route={{ name: 'today' }} />
        </>,
        { wrapper },
      );

      // Wait for the initial area list to land in the sidebar.
      await waitFor(() => {
        expect(getSidebarAreaNavButton(/StaleAtlas/)).toBeInTheDocument();
      });

      // Fire the WS event. The write-through must repaint the
      // sidebar before any refetch could resolve (the second
      // listAreas stub never resolves, exactly to prove this).
      await act(async () => {
        fake.emit({
          ev: 'area.updated',
          data: {
            id: 'c2',
            name: 'FreshAtlas',
            color: '#5a9',
            sort: 0,
            kind: 'user',
            created_at: 1,
            updated_at: 99,
          },
        });
        await Promise.resolve();
      });

      await waitFor(() => {
        expect(
          getSidebarAreaNavButton(/FreshAtlas/),
          'sidebar must reflect new name from WS write-through',
        ).toBeInTheDocument();
      });
      expect(querySidebarAreaNavButton(/StaleAtlas/)).toBeNull();
    } finally {
      Object.assign(stream, orig);
    }
  });

  it('after the server response lands, the sidebar continues to show the new name', async () => {
    // Counterpart to the mid-flight test: pin the post-settle state.
    // `onSettled` invalidates ['areas'] which triggers a refetch; the
    // refetch returns the server-confirmed payload. If structural
    // sharing or an observer subscription regression silently dropped
    // the refetch's notification to the sidebar, this would catch it.
    const client = makeClient();
    const seedBefore = [
      makeKernelArea('c1', 'KeepMe'),
      makeKernelArea('c2', 'BeforeRename'),
    ];
    const seedAfter = [
      seedBefore[0],
      { ...seedBefore[1], name: 'AfterRename', updated_at: 99 },
    ];
    (api.listAreas as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(seedBefore)
      .mockResolvedValueOnce(seedAfter);
    (api.updateArea as ReturnType<typeof vi.fn>).mockResolvedValue(seedAfter[1]);

    let triggerRename: ((id: string, name: string) => Promise<void>) | null = null;
    const wrapper = wrap(client);
    render(
      <>
        <SidebarHost route={{ name: 'today' }} />
        <Renamer onReady={(fn) => { triggerRename = fn; }} />
      </>,
      { wrapper },
    );

    await waitFor(() => {
      expect(getSidebarAreaNavButton(/BeforeRename/)).toBeInTheDocument();
    });

    await act(async () => {
      await triggerRename!('c2', 'AfterRename');
      // Allow onSettled's invalidate → refetch chain to drain.
      await Promise.resolve();
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(getSidebarAreaNavButton(/AfterRename/)).toBeInTheDocument();
    });
    expect(querySidebarAreaNavButton(/BeforeRename/)).toBeNull();
  });
});
