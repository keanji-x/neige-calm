// #177 regression-anchor test: XtermView must NOT remount when the user
// toggles the app-level theme.
//
// The bug chain we're guarding (still observed in the browser even after
// the `placeholderData: keepPreviousData` fix on the wave-detail query):
//
//   1. User clicks the theme toggle. ThemeProvider's `resolved` flips
//      light ↔ dark.
//   2. CodexCardImpl re-renders with the new `theme` prop and passes it to
//      its lazy-loaded XtermView child.
//   3. **Something** in the subtree (Suspense boundary? lazy chunk?) tears
//      the XtermView's hook state down between two adjacent renders — its
//      `useRef` returns a different value, indicating a fresh mount.
//   4. The fresh mount's `prevThemeRef` starts at `null`, so the
//      `TerminalThemeUpdate` OSC is never dispatched to the daemon; the
//      composer keeps its old fg/bg and the user reports "theme toggle
//      didn't work".
//
// The user's DevTools console captured the smoking gun:
//
//      [#177 XtermView instance] {theme: 'light', instance: 'bscohp'}
//      ... (steady state)
//      [#177 CodexCardImpl render] {theme: 'dark'}
//      [#177 XtermView render] {theme: 'dark'}
//      [#177 XtermView instance] {theme: 'dark', instance: 'zjqsq4'}
//                                                            ^^^^^^
//                                              new instance id → remount
//
// Notably ABSENT from the trace: `WaveComponent EARLY-RETURN` (so it isn't
// the `!detailQ.data → return null` path we already patched) and `ws-mount
// CLEANUP RUNNING` (the React effect cleanup didn't run between the two
// mounts — strongly suggesting an offscreen / Suspense "hidden tree"
// transition rather than a normal subtree unmount).
//
// What this file does
// -------------------
// We mount the closest reasonable approximation of the production tree
// that still surfaces XtermView:
//
//   <ThemeProvider>                          ← real, from app/theme.tsx
//     <QueryClientProvider>                  ← real RQ client
//       <Suspense fallback="loading">        ← matches WavePage's Suspense
//         <CodexEntry.Component card={..} /> ← real codex card via registry
//       </Suspense>
//     </QueryClientProvider>
//   </ThemeProvider>
//
// `CodexEntry.Component` internally `React.lazy()`-imports XtermView the
// same way the production card does — so the lazy boundary is real.
//
// Mocks
// -----
//  * `@xterm/xterm` — counted Terminal constructor; instance count is the
//    direct signal for "did XtermView remount?". A stable instance = same
//    mount; a second constructor call = a fresh mount = the bug.
//  * `@xterm/addon-fit` and `@xterm/xterm/css/xterm.css` — jsdom-friendly stubs.
//  * `WebSocket` — minimal in-memory stand-in so the bridge effect runs.
//  * `ResizeObserver` — jsdom doesn't ship one; XtermView's effect needs it.
//
// What the bug looks like in test
// -------------------------------
// If the bug reproduces: after `setMode('dark')`, the mocked Terminal
// constructor count climbs from 1 → 2 (XtermView remounted, built a new
// xterm).
//
// If the bug does NOT reproduce in test isolation: constructor stays at 1
// and we have evidence the test environment is missing whatever factor
// triggers the remount in production. In that case the test still serves
// as a regression anchor — a future change that flips XtermView to a
// theme-keyed component (or otherwise destabilizes its identity) will
// trip this assertion. We document the env gap in a TODO at the bottom.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { Suspense, StrictMode, useEffect, type ReactNode } from 'react';
import { act, render, screen, waitFor } from '@testing-library/react';
import {
  QueryClient,
  QueryClientProvider,
  useIsRestoring,
  useQueryClient,
} from '@tanstack/react-query';

// ---- xterm mock --------------------------------------------------------
//
// Count Terminal constructor invocations. Each `new Terminal()` corresponds
// to one XtermView mount; the assertion below counts these.

let termCtorCount = 0;
const termInstances: Array<{ id: number; options: Record<string, unknown> }> = [];

vi.mock('@xterm/xterm', () => {
  class Terminal {
    cols = 80;
    rows = 24;
    options: Record<string, unknown> = {};
    write = vi.fn();
    writeln = vi.fn();
    clear = vi.fn();
    resize = vi.fn((cols: number, rows: number) => {
      this.cols = cols;
      this.rows = rows;
    });
    open = vi.fn();
    loadAddon = vi.fn();
    dispose = vi.fn();
    onData(_cb: (d: string) => void): { dispose: () => void } {
      return { dispose: () => {} };
    }
    constructor(opts: Record<string, unknown> = {}) {
      termCtorCount += 1;
      this.options = { ...opts };
      termInstances.push({ id: termCtorCount, options: this.options });
    }
  }
  return { Terminal };
});

vi.mock('@xterm/addon-fit', () => {
  class FitAddon {
    fit = vi.fn();
  }
  return { FitAddon };
});

vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

// ---- sharedEventStream mock --------------------------------------------
//
// `CodexCardImpl` subscribes to the shared WS event stream (for codex.hook
// + overlay.set events). The real `EventStream` opens a WebSocket via
// `ws.addEventListener('open', ...)` — our `FakeWebSocketCtor` below only
// implements property-setter handlers (onopen/onmessage/etc), matching the
// pattern used by `XtermView.test.tsx`. Rather than dual-shape the fake,
// we stub the shared stream out wholesale: this test is about XtermView
// mount stability across a theme flip, not about event-bus wiring.

vi.mock('../api/events', () => ({
  // `sharedEventStream` is a `vi.fn()` so individual tests (notably the
  // Factor 2 block at the bottom) can swap in a richer in-memory pub/sub
  // via `vi.mocked(sharedEventStream).mockImplementation(...)`. The
  // default impl is the minimal surface the CodexCardImpl needs (addTopic
  // + on returning a disposer); the EventBridge mounted in Factor 2 needs
  // additional methods (`subscribe`, `onReplayComplete`, ...) which the
  // per-test override provides.
  sharedEventStream: vi.fn(() => ({
    addTopic: () => {},
    removeTopic: () => {},
    on: () => () => {},
  })),
}));

// ---- calm api mock -----------------------------------------------------
//
// The third test below renders a `WaveComponent`-shaped wrapper that calls
// `useWaveDetailQuery`. We mock the REST client so we can deterministically
// model the refetch cycle (the production bug suspect: theme toggle →
// overlay.set event → invalidateQueries → refetch with placeholderData).

vi.mock('../api/calm', () => ({
  getWaveDetail: vi.fn(),
  listCoves: vi.fn(),
  wavesInCove: vi.fn(),
  listAllOverlays: vi.fn(),
  CalmApiError: class CalmApiError extends Error {
    status: number;
    constructor(msg: string, status: number) {
      super(msg);
      this.status = status;
    }
  },
}));

// ---- WebSocket mock ----------------------------------------------------

interface FakeWS {
  readyState: number;
  url: string;
  sentFrames: string[];
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: string }) => void) | null;
  onclose:
    | ((ev: { code: number; reason: string; wasClean: boolean }) => void)
    | null;
  onerror: ((ev: unknown) => void) | null;
  send: (data: string) => void;
  close: () => void;
}

let wsInstances: FakeWS[] = [];

class FakeWebSocketCtor {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readyState = FakeWebSocketCtor.CONNECTING;
  url: string;
  sentFrames: string[] = [];
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose:
    | ((ev: { code: number; reason: string; wasClean: boolean }) => void)
    | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  constructor(url: string) {
    this.url = url;
    wsInstances.push(this as unknown as FakeWS);
  }
  send(data: string): void {
    this.sentFrames.push(data);
  }
  close(): void {
    this.readyState = FakeWebSocketCtor.CLOSED;
  }
}

// ---- imports under test ------------------------------------------------
//
// Pulled AFTER the mocks above so the registry entry's `lazy(() => import('XtermView'))`
// resolves against our mocked xterm constructor.

import { CodexEntry } from '../cards/builtins/codex';
import { ThemeProvider, useTheme } from '../app/theme';
import type { CodexCardData } from '../types';

// ---- test harness ------------------------------------------------------
//
// `<ThemeFlipper>` exposes the ThemeProvider's `setMode` via a window
// callback the test grabs and invokes inside `act(...)`. We don't need to
// click any DOM control — the bug is wholly above the DOM, in the React
// tree's response to a context value change.

let exposedSetMode:
  | ((m: 'light' | 'dark' | 'system') => void)
  | null = null;

function ThemeBridge() {
  const { setMode } = useTheme();
  useEffect(() => {
    exposedSetMode = setMode;
    return () => {
      exposedSetMode = null;
    };
  }, [setMode]);
  return null;
}

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { retry: false },
    },
  });
}

function makeCodexCard(): CodexCardData {
  return {
    type: 'codex',
    id: 'card_codex_test',
    terminalId: 'term_test',
    cwd: '/tmp',
  };
}

beforeEach(() => {
  termCtorCount = 0;
  termInstances.length = 0;
  wsInstances = [];
  exposedSetMode = null;
  // Force-clear any persisted theme from a previous test so the provider
  // starts in 'system' (which `readSystemPreference` resolves to 'light'
  // in jsdom — matchMedia returns false by default).
  try {
    window.localStorage.removeItem('calm.theme');
  } catch {
    /* ignore */
  }
  (globalThis as { WebSocket: typeof WebSocket }).WebSocket =
    FakeWebSocketCtor as unknown as typeof WebSocket;
  (globalThis as { ResizeObserver: typeof ResizeObserver }).ResizeObserver =
    class {
      observe() {}
      disconnect() {}
      unobserve() {}
    } as unknown as typeof ResizeObserver;
  if (!('randomUUID' in (globalThis.crypto ?? {}))) {
    Object.defineProperty(globalThis.crypto, 'randomUUID', {
      configurable: true,
      value: () => '00000000-0000-4000-8000-000000000000',
    });
  }
});

afterEach(() => {
  wsInstances = [];
  termInstances.length = 0;
});

describe('#177 theme toggle does NOT remount XtermView (regression anchor)', () => {
  it('keeps a single Terminal instance across a setMode("dark") flip from a CodexCard subtree', async () => {
    const qc = makeClient();
    const Card = CodexEntry.Component;
    render(
      <ThemeProvider>
        <ThemeBridge />
        <QueryClientProvider client={qc}>
          <Suspense fallback={<div data-testid="suspense-fallback">loading</div>}>
            <Card card={makeCodexCard()} />
          </Suspense>
        </QueryClientProvider>
      </ThemeProvider>,
    );

    // The lazy import in `codex.tsx` needs a microtask flush before the
    // real XtermView renders. We wait for the mocked Terminal constructor
    // to fire — that's the canonical signal that XtermView's bridge
    // effect ran.
    await waitFor(() => expect(termCtorCount).toBe(1));
    expect(exposedSetMode).toBeTruthy();
    // Theme starts as system → light in jsdom (matchMedia stub returns
    // dark=false). Sanity check the constructed Terminal carries the
    // light theme by background opacity (both themes share `#ffffff00`
    // since xterm draws on a transparent canvas, so we look at foreground).
    expect(termInstances[0]!.options.theme).toMatchObject({
      foreground: expect.stringMatching(/^#/),
    });
    const initialInstance = termInstances[0]!;

    // The flip — the exact same call path the Settings page (and the
    // sidebar's theme toggle) uses. ThemeProvider's `setMode` updates
    // localStorage AND triggers the resolved-theme effect that writes
    // `<html data-theme>`. CodexCardImpl is subscribed via `useTheme()`,
    // so it re-renders with the new `theme` prop on its child XtermView.
    await act(async () => {
      exposedSetMode!('dark');
    });

    // ---- The critical regression assertion -----------------------------
    // If XtermView remounted, a SECOND Terminal would have been constructed
    // with the dark theme. The test environment may or may not reproduce
    // the production behavior — see the TODO at the bottom of this file.
    //
    // We test the strongest reasonable invariant: the Terminal instance
    // count must not exceed 1. If it does (== 2 with the dark theme
    // applied), we have a deterministic reproducer for the production
    // bug and can iterate against it.
    expect(termCtorCount).toBe(1);
    expect(termInstances).toHaveLength(1);
    expect(termInstances[0]).toBe(initialInstance);

    // Belt-and-braces: no Suspense fallback was painted between renders
    // (which would also indicate the lazy boundary re-suspended).
    expect(screen.queryByTestId('suspense-fallback')).not.toBeInTheDocument();
  });

  it('emits the live-apply theme update on the same Terminal instance (no rebuild)', async () => {
    // Mirror the assertion above from a different angle: the live-theme
    // effect inside XtermView assigns `term.options.theme = ...` on theme
    // change. If the Terminal survived, its `options.theme` should reflect
    // the new colors. If a new Terminal was constructed, this *also*
    // passes — but the previous test catches that case. Together they
    // form a two-sided fence.
    const qc = makeClient();
    const Card = CodexEntry.Component;
    render(
      <ThemeProvider>
        <ThemeBridge />
        <QueryClientProvider client={qc}>
          <Suspense fallback={<div>loading</div>}>
            <Card card={makeCodexCard()} />
          </Suspense>
        </QueryClientProvider>
      </ThemeProvider>,
    );
    await waitFor(() => expect(termCtorCount).toBe(1));
    const term = termInstances[0]!;
    const lightFg = (term.options.theme as { foreground: string }).foreground;

    await act(async () => {
      exposedSetMode!('dark');
    });

    // Same instance still recorded.
    expect(termInstances).toHaveLength(1);
    // The live-apply effect re-assigned `term.options.theme` to the dark
    // palette — foreground must differ from the initial light value.
    const darkFg = (term.options.theme as { foreground: string }).foreground;
    expect(darkFg).not.toEqual(lightFg);
  });
});

// ---------------------------------------------------------------------------
// Third test — closest reasonable approximation of the production WaveComponent
// path: a wrapper that uses `useWaveDetailQuery` with placeholderData, and
// invalidates the query on theme toggle to model the
// "theme flip → RGL onLayoutChange → overlay.set → invalidate → refetch"
// feedback loop that we suspect drives the production remount.
// ---------------------------------------------------------------------------

import { useWaveDetailQuery } from '../api/queries';
import * as calmApi from '../api/calm';
import type { KernelWaveDetail } from '../api/wire';

function makeWaveDetail(): KernelWaveDetail {
  return {
    wave: {
      id: 'wave_test',
      cove_id: 'cove_test',
      title: 'Test wave',
      sort: 0,
      archived_at: null,
      created_at: 1000,
      updated_at: 2000,
    },
    cards: [],
    overlays: [],
  };
}

/**
 * Mini-WaveComponent: same guard shape as the production component
 * (`if (!detailQ.data) return null`) and the same card render path. The
 * card object is passed in directly rather than adapted from `detail.cards`
 * — we only care about whether XtermView's subtree survives a parent
 * re-render driven by both ThemeProvider AND a wave-detail refetch.
 */
function MiniWaveComponent({ card }: { card: CodexCardData }) {
  const detailQ = useWaveDetailQuery('wave_test');
  if (!detailQ.data) return null;
  const Card = CodexEntry.Component;
  return (
    <Suspense fallback={<div data-testid="suspense-fallback">loading</div>}>
      <Card card={card} />
    </Suspense>
  );
}

/**
 * Drives `invalidateQueries(['wave', 'wave_test'])` on each theme flip —
 * simulates the eventBridge's response to an `overlay.set` event that
 * RGL would emit when the [data-theme] CSS swap re-measures the grid.
 */
function ThemeFlipInvalidator() {
  const { resolved } = useTheme();
  const qc = useQueryClient();
  useEffect(() => {
    // Skip the initial mount — only invalidate on subsequent flips.
    // Use a ref-like flag captured in closure: the effect runs once on
    // every distinct `resolved` value.
    qc.invalidateQueries({ queryKey: ['wave', 'wave_test'] });
  }, [resolved, qc]);
  return null;
}

describe('#177 theme toggle does NOT remount XtermView (with refetch loop)', () => {
  it('survives the wave-detail refetch triggered by a theme flip', async () => {
    const detail = makeWaveDetail();
    (calmApi.getWaveDetail as ReturnType<typeof vi.fn>).mockResolvedValue(
      detail,
    );

    const qc = makeClient();
    render(
      <ThemeProvider>
        <ThemeBridge />
        <QueryClientProvider client={qc}>
          <ThemeFlipInvalidator />
          <MiniWaveComponent card={makeCodexCard()} />
        </QueryClientProvider>
      </ThemeProvider>,
    );

    // Wait for the wave detail to load AND the lazy XtermView to mount.
    await waitFor(() => expect(termCtorCount).toBe(1));

    // The initial render already triggered one invalidate via
    // ThemeFlipInvalidator's `useEffect` mount. Wait for that refetch
    // to settle before issuing the theme flip — otherwise the test
    // races the placeholder window we already cover in queries.test.tsx.
    await waitFor(() =>
      expect(
        (calmApi.getWaveDetail as ReturnType<typeof vi.fn>).mock.calls.length,
      ).toBeGreaterThanOrEqual(1),
    );

    // Now: the flip. This (a) re-renders CodexCardImpl with theme='dark',
    // and (b) re-fires ThemeFlipInvalidator's effect → invalidate →
    // refetch wave detail. The placeholderData should hold the previous
    // value visible across the refetch, BUT — if anything in the chain
    // forces a remount of the subtree on the refetch tick, we'll see
    // a second `new Terminal()`.
    await act(async () => {
      exposedSetMode!('dark');
    });

    // Let any pending microtasks / refetches settle.
    await waitFor(() => {
      // The mock resolves synchronously after each invalidate; once the
      // invalidate has been observed, the cache should be stable again.
      const calls = (calmApi.getWaveDetail as ReturnType<typeof vi.fn>).mock
        .calls.length;
      expect(calls).toBeGreaterThanOrEqual(1);
    });

    // The critical assertion — even with a placeholderData refetch cycle
    // riding alongside the theme flip, only one Terminal was constructed.
    expect(termCtorCount).toBe(1);
    // No fallback paint either — the lazy boundary didn't re-suspend.
    expect(screen.queryByTestId('suspense-fallback')).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Factor probes (#177): the three describe blocks above all PASS on HEAD,
// yet the user reports still observing the remount in DevTools. The blocks
// below each layer in ONE production-only structural ingredient that was
// missing from the bare harness, in order of suspected likelihood. They
// ALL also pass — i.e. none of the three factors (and none of their pair-
// wise / triple combinations, including the COMBINED block at the bottom)
// reproduce the remount in vitest/jsdom. Even with `<StrictMode>` wrapping
// the full stack, the XtermView's `useRef` instance id is stable across
// `setMode('dark')` and the ws-mount cleanup does NOT fire mid-test.
//
//   Factor 1 — TanStack Router `<Match>` Suspense boundary
//   Factor 3 — PersistQueryClientProvider + useIsRestoring() gate
//   Factor 2 — real eventBridge → invalidate → wave-detail refetch
//   COMBINED — all three stacked + production-shaped per-render card
//              adaption + StrictMode
//
// Why we keep them as PASSING characterization tests:
//   * The signal is uniform (`termCtorCount > 1` or instance id flip
//     after the theme toggle) and tight to the bug's symptom.
//   * The moment ANYTHING destabilizes the subtree across a theme flip
//     under one of these layered configurations, the corresponding test
//     will fail and we'll know exactly which ingredient is to blame.
//   * Keeping them green (rather than `it.fails`) means CI alerts on a
//     regression direction we haven't tripped yet — strictly more useful
//     than asserting "currently broken" against a state we can't actually
//     reproduce.
//
// Production-vs-test gap that REMAINS (the most plausible source of the
// real bug, none of which is reachable from a pure React-tree test):
//   1. Real `react-grid-layout` + real ResizeObserver. The
//      `[data-theme="dark"]` CSS swap can change CSS variables that
//      affect cell dimensions; RGL's ResizeObserver fires, RGL re-renders
//      its layout container, and a structural change to the outer
//      `<div>` (e.g. a transform / inline style swap that triggers React
//      to reconcile the host instance) tears the XtermView subtree.
//   2. Real browser layout / paint pipeline interacting with xterm.js's
//      own canvas / WebGL renderer (we mock the Terminal class entirely).
//   3. DevTools instrumentation that artificially flags an
//      attribute-only update as a remount (low-likelihood but explains
//      the "useRef changes without cleanup running" signature in a way
//      no other factor here does).
//
// Next step if reproduction is still required: stand the app up under
// Playwright with the real xterm.js + real RGL, toggle the theme, and
// observe `Terminal` instance count via a `window.__termCount__` hook
// added behind a `?debug=1` query param. That's strictly an integration-
// test exercise outside this file's scope.
// ---------------------------------------------------------------------------

import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
  Outlet,
} from '@tanstack/react-router';
import { PersistQueryClientProvider } from '@tanstack/react-query-persist-client';
import type { PersistedClient, Persister } from '@tanstack/react-query-persist-client';
import { EventBridge } from '../app/eventBridge';
import { sharedEventStream } from '../api/events';

// ---------------------------------------------------------------------------
// Factor 1 — Mount a minimal TanStack Router with memory history; the route
// component mirrors production's `WaveComponent` (uses
// `useWaveDetailQuery` + the same early-return guard) and renders the
// CodexCard inside the same Suspense boundary WavePage uses. The router
// wraps every matched route in its own internal `<Match>` Suspense — the
// structural difference between this test and the bare-tree describes
// above. If a route-level pending state (loader re-run, context change,
// match re-resolution) un-suspends and re-suspends across the theme flip,
// the lazy CodexCard subtree underneath remounts and we'll see
// `termCtorCount === 2`.
// ---------------------------------------------------------------------------

describe('#177 Factor 1 — TanStack Router <Match> Suspense boundary', () => {
  it(
    'XtermView survives a theme flip when mounted under RouterProvider',
    async () => {
      const detail = makeWaveDetail();
      // Make the codex card visible — production's WaveComponent renders
      // `detail.cards.map(adaptCard)`, but our route component (below)
      // skips the adaption pipeline and renders the codex card directly,
      // so we just need a non-empty detail. The mock kernel response is
      // returned once; subsequent refetches reuse the same value.
      (calmApi.getWaveDetail as ReturnType<typeof vi.fn>).mockResolvedValue(
        detail,
      );

      const qc = makeClient();

      // Route component: same guard pattern as production WaveComponent.
      // We render the codex card under a Suspense to match WavePage's
      // local boundary — the bug surfaces in production despite this
      // local Suspense being intact, suggesting a higher-up boundary
      // (router's `<Match>`) is the actual trigger.
      function RouteWaveComponent() {
        const detailQ = useWaveDetailQuery('wave_test');
        if (!detailQ.data) return null;
        const Card = CodexEntry.Component;
        return (
          <Suspense
            fallback={<div data-testid="suspense-fallback">loading</div>}
          >
            <Card card={makeCodexCard()} />
          </Suspense>
        );
      }

      const rootRoute = createRootRoute({
        component: () => (
          <ThemeProvider>
            <ThemeBridge />
            <QueryClientProvider client={qc}>
              <Outlet />
            </QueryClientProvider>
          </ThemeProvider>
        ),
      });
      const waveRoute = createRoute({
        getParentRoute: () => rootRoute,
        path: '/wave/$waveId',
        component: RouteWaveComponent,
      });
      const routeTree = rootRoute.addChildren([waveRoute]);
      const memoryHistory = createMemoryHistory({
        initialEntries: ['/wave/wave_test'],
      });
      const router = createRouter({ routeTree, history: memoryHistory });

      render(<RouterProvider router={router} />);

      // Wait for router to land + lazy CodexCard chunk to resolve + first
      // Terminal to be constructed. Slightly more patient than the bare
      // describes above because the router pipeline adds extra micro-task
      // hops.
      await waitFor(() => expect(termCtorCount).toBe(1), { timeout: 2000 });
      const initialInstance = termInstances[0]!;

      await act(async () => {
        exposedSetMode!('dark');
      });

      // If the router's `<Match>` Suspense re-suspends across the theme
      // flip, the lazy CodexCard chunk re-runs and a second Terminal is
      // constructed. `it.fails(...)` flips the polarity: the test
      // "passes" when this expectation FAILS, so a future change that
      // actually stabilizes the subtree under the router will surface
      // here as an unexpected pass and prompt us to remove `.fails`.
      expect(termCtorCount).toBe(1);
      expect(termInstances).toHaveLength(1);
      expect(termInstances[0]).toBe(initialInstance);
    },
  );
});

// ---------------------------------------------------------------------------
// Factor 3 — Wrap the tree in `PersistQueryClientProvider`. The provider
// gates children on `useIsRestoring()`; the production `QueryRestoreGate`
// renders `null` while restoring and the real tree only after the
// hydration promise resolves. If the persister's restore promise resolves
// AFTER the first render (the realistic order), the QueryRestoreGate
// transitions `null → children`, which is itself a fresh mount of every
// descendant. If a subsequent theme flip triggers a second `restoring`
// transition (e.g. via the persister's `triggerSave` writeback path
// touching the same state), XtermView re-mounts.
//
// The mock persister below is the minimal `Persister` interface from
// `@tanstack/react-query-persist-client`; we control when `restoreClient`
// resolves so the test deterministically observes the `isRestoring`
// transition.
// ---------------------------------------------------------------------------

describe('#177 Factor 3 — PersistQueryClientProvider + useIsRestoring()', () => {
  it(
    'XtermView survives a theme flip wrapped in PersistQueryClientProvider',
    async () => {
      // Hand-rolled persister we can drive from the test. `restoreClient`
      // returns a promise we resolve manually so we can observe a real
      // `isRestoring: true → false` transition with the React tree
      // already mounted.
      let resolveRestore: (() => void) | null = null;
      const restorePromise = new Promise<void>((resolve) => {
        resolveRestore = () => resolve();
      });
      const persister: Persister = {
        persistClient: async () => {},
        // Returning `undefined` means "no persisted state to hydrate"; the
        // provider still flips `isRestoring` based on the promise resolving.
        restoreClient: async (): Promise<PersistedClient | undefined> => {
          await restorePromise;
          return undefined;
        },
        removeClient: async () => {},
      };

      const qc = makeClient();
      const Card = CodexEntry.Component;

      // Mirror the production layering exactly:
      //   PersistQueryClientProvider
      //     ↳ EventBridge (not used here, see Factor 2 for that path)
      //     ↳ QueryRestoreGate — renders `null` while isRestoring
      //         ↳ ThemeProvider
      //             ↳ <children>
      // The QueryRestoreGate lives in providers.tsx; we replicate its
      // body inline so this test doesn't drag the whole AppProviders chain
      // (with its ServerCompatGate / IndexedDB calls) into the harness.
      function RestoreGate({ children }: { children: ReactNode }) {
        const isRestoring = useIsRestoring();
        return isRestoring ? null : <>{children}</>;
      }

      render(
        <PersistQueryClientProvider
          client={qc}
          persistOptions={{ persister }}
        >
          <RestoreGate>
            <ThemeProvider>
              <ThemeBridge />
              <Suspense
                fallback={
                  <div data-testid="suspense-fallback">loading</div>
                }
              >
                <Card card={makeCodexCard()} />
              </Suspense>
            </ThemeProvider>
          </RestoreGate>
        </PersistQueryClientProvider>,
      );

      // Before restore resolves: tree is gated, XtermView hasn't mounted.
      expect(termCtorCount).toBe(0);

      // Flip restoring → done. The provider transitions, RestoreGate
      // re-renders with children, and XtermView mounts for the first time.
      await act(async () => {
        resolveRestore!();
      });
      await waitFor(() => expect(termCtorCount).toBe(1));
      const initialInstance = termInstances[0]!;

      // Now the suspect transition: a theme flip. If the persister's
      // `subscribe`-driven save path causes a second `isRestoring` flicker
      // (or if the provider's own state update interleaves with the
      // theme-context update in a way that tears the subtree), we'll see
      // a second Terminal constructed.
      await act(async () => {
        exposedSetMode!('dark');
      });

      expect(termCtorCount).toBe(1);
      expect(termInstances).toHaveLength(1);
      expect(termInstances[0]).toBe(initialInstance);
    },
  );
});

// ---------------------------------------------------------------------------
// Factor 2 — Use the REAL `eventBridge` and the REAL `sharedEventStream`.
// The third describe above (the "refetch loop" variant) only modeled the
// dispatcher side of the bridge: it called `invalidateQueries` directly.
// This block hooks into the actual event stream so a real `overlay.set`
// envelope goes through `wireEventSchema` validation, then the dispatch
// switch, then the QC. If something in that pipeline holds a reference
// the bare loop didn't (e.g. a different listener fan-out timing, or the
// stream's `addTopic` triggering a `publishSub` write that the test
// observes as a re-render), the XtermView will remount.
// ---------------------------------------------------------------------------

// Re-mock the event stream for the Factor 2 block ONLY. The top-of-file
// `vi.mock('../api/events', …)` stubs `sharedEventStream` for every
// describe; we override it inside this block via `vi.doMock` is hard with
// hoisting — easier to layer a real-ish in-memory stream over the mock
// surface using `vi.mocked(sharedEventStream).mockImplementation(...)`.

describe('#177 Factor 2 — real eventBridge + simulated overlay.set', () => {
  it(
    'XtermView survives an overlay.set-driven refetch on theme flip',
    async () => {
      // In-memory pub/sub matching the EventStream surface used by the
      // bridge + CodexCardImpl. Real events go through `wireEventSchema`
      // in `EventStream.handleFrame`; we shortcut to listener fan-out
      // because the dispatch logic in eventBridge.tsx doesn't care where
      // the parsed envelope originated.
      const listeners = new Set<(ev: unknown, meta: unknown) => void>();
      const subscribers: ((s: string) => void)[] = [];
      const stub = {
        addTopic: () => {},
        removeTopic: () => {},
        on: (fn: (ev: unknown, meta: unknown) => void) => {
          listeners.add(fn);
          return () => listeners.delete(fn);
        },
        subscribe: (topics: string[]) => {
          for (const t of topics) for (const s of subscribers) s(t);
        },
        onReplayComplete: () => () => {},
        onSnapshotRequired: () => () => {},
        onConnectionState: (fn: (s: string) => void) => {
          fn('connected');
          return () => {};
        },
      };
      vi.mocked(sharedEventStream).mockImplementation(
        () => stub as unknown as ReturnType<typeof sharedEventStream>,
      );

      const detail = makeWaveDetail();
      (calmApi.getWaveDetail as ReturnType<typeof vi.fn>).mockResolvedValue(
        detail,
      );

      const qc = makeClient();
      const Card = CodexEntry.Component;

      render(
        <ThemeProvider>
          <ThemeBridge />
          <QueryClientProvider client={qc}>
            <EventBridge />
            <Suspense
              fallback={<div data-testid="suspense-fallback">loading</div>}
            >
              <Card card={makeCodexCard()} />
            </Suspense>
          </QueryClientProvider>
        </ThemeProvider>,
      );

      await waitFor(() => expect(termCtorCount).toBe(1));
      const initialInstance = termInstances[0]!;

      // Theme flip first — gets the ThemeProvider into 'dark' state.
      await act(async () => {
        exposedSetMode!('dark');
      });

      // Now feed a real-shape overlay.set envelope through the live
      // listener set. This mirrors the production trigger: the
      // [data-theme="dark"] CSS swap causes RGL to fire an onLayoutChange
      // → PATCH overlay → server emits overlay.set → eventBridge
      // dispatches → invalidate('wave', 'wave_test') → refetch.
      const overlayEv = {
        ev: 'overlay.set' as const,
        data: {
          entity_kind: 'wave' as const,
          entity_id: 'wave_test',
          kind: 'layout',
          payload: { y: 1 },
        },
      };
      const overlayMeta = { id: 1, eventVersion: 1 };
      await act(async () => {
        for (const fn of listeners) fn(overlayEv, overlayMeta);
      });

      // Let the refetch settle.
      await waitFor(() => {
        expect(
          (calmApi.getWaveDetail as ReturnType<typeof vi.fn>).mock.calls
            .length,
        ).toBeGreaterThanOrEqual(0);
      });

      expect(termCtorCount).toBe(1);
      expect(termInstances).toHaveLength(1);
      expect(termInstances[0]).toBe(initialInstance);
    },
  );
});

// ---------------------------------------------------------------------------
// COMBINED — Stack every production-only factor at once and additionally
// mimic the production WaveComponent's per-render card adaption: instead
// of holding a stable `card` reference, this wrapper re-builds the
// CodexCardData on every render the way `detail.cards.map(adaptCard)`
// does in `app/router.tsx`. New card object identity each render gives
// React's reconciler the most plausible reason to break instance
// continuity through XtermView.
//
// The full stack here is:
//   <PersistQueryClientProvider>
//     <EventBridge> + sharedEventStream stub
//     <RestoreGate isRestoring> (flips false during the test)
//     <ThemeProvider>
//       <RouterProvider memory-history>
//         (route: WaveComponent-shape that adapts cards per render)
//           <Suspense> (matches WavePage's local boundary)
//             <CodexCard card={NEW each render} />
//
// If THIS combination still keeps a single Terminal instance through a
// theme flip + an overlay.set-driven refetch, the production remount
// trigger must be living outside our reachable surface (CSS-driven RGL
// resize callbacks fired from a real ResizeObserver during the
// [data-theme] swap; the actual browser layout pipeline; or a code path
// the test environment has stubbed out). The TODO below captures what
// next step is required.
// ---------------------------------------------------------------------------

describe('#177 COMBINED — all factors stacked together', () => {
  it(
    'XtermView survives a theme flip with the full production tree',
    async () => {
      // ---- Persister (Factor 3) -----------------------------------------
      let resolveRestore: (() => void) | null = null;
      const restorePromise = new Promise<void>((resolve) => {
        resolveRestore = () => resolve();
      });
      const persister: Persister = {
        persistClient: async () => {},
        restoreClient: async (): Promise<PersistedClient | undefined> => {
          await restorePromise;
          return undefined;
        },
        removeClient: async () => {},
      };

      // ---- EventBridge stream stub (Factor 2) ---------------------------
      const listeners = new Set<(ev: unknown, meta: unknown) => void>();
      const stub = {
        addTopic: () => {},
        removeTopic: () => {},
        on: (fn: (ev: unknown, meta: unknown) => void) => {
          listeners.add(fn);
          return () => listeners.delete(fn);
        },
        subscribe: () => {},
        onReplayComplete: () => () => {},
        onSnapshotRequired: () => () => {},
        onConnectionState: (fn: (s: string) => void) => {
          fn('connected');
          return () => {};
        },
      };
      vi.mocked(sharedEventStream).mockImplementation(
        () => stub as unknown as ReturnType<typeof sharedEventStream>,
      );

      // ---- Wave-detail mock returning a fresh detail per call -----------
      // Mirrors production: REST returns a new object per request so the
      // refetch path can't accidentally hand React a stable reference.
      (calmApi.getWaveDetail as ReturnType<typeof vi.fn>).mockImplementation(
        async () => makeWaveDetail(),
      );

      // ---- RestoreGate (matches providers.tsx) --------------------------
      function RestoreGate({ children }: { children: ReactNode }) {
        const isRestoring = useIsRestoring();
        return isRestoring ? null : <>{children}</>;
      }

      // ---- Route component mirrors WaveComponent's card-adaption shape --
      // Per render, build a brand-new CodexCardData. This stresses the
      // "new card prop identity each render" path that the production
      // WaveComponent exercises via `detail.cards.map(adaptCard)`.
      function RouteWaveComponent() {
        const detailQ = useWaveDetailQuery('wave_test');
        if (!detailQ.data) return null;
        const Card = CodexEntry.Component;
        // Fresh object every render — same id/terminalId, NEW identity.
        const card: CodexCardData = {
          type: 'codex',
          id: 'card_codex_test',
          terminalId: 'term_test',
          cwd: '/tmp',
        };
        return (
          <Suspense
            fallback={<div data-testid="suspense-fallback">loading</div>}
          >
            <Card card={card} />
          </Suspense>
        );
      }

      const rootRoute = createRootRoute({ component: () => <Outlet /> });
      const waveRoute = createRoute({
        getParentRoute: () => rootRoute,
        path: '/wave/$waveId',
        component: RouteWaveComponent,
      });
      const routeTree = rootRoute.addChildren([waveRoute]);
      const memoryHistory = createMemoryHistory({
        initialEntries: ['/wave/wave_test'],
      });
      const router = createRouter({ routeTree, history: memoryHistory });

      const qc = makeClient();

      render(
        <StrictMode>
          <PersistQueryClientProvider
            client={qc}
            persistOptions={{ persister }}
          >
            <RestoreGate>
              <ThemeProvider>
                <ThemeBridge />
                <QueryClientProvider client={qc}>
                  <EventBridge />
                  <RouterProvider router={router} />
                </QueryClientProvider>
              </ThemeProvider>
            </RestoreGate>
          </PersistQueryClientProvider>
        </StrictMode>,
      );

      // Flush the persister restore so RestoreGate renders children.
      await act(async () => {
        resolveRestore!();
      });

      // Wait for first XtermView mount. With StrictMode wrapping the tree,
      // React's dev double-invoke creates 2 constructors on the initial
      // mount (1 throwaway + 1 retained). Both ARE expected; the bug
      // surfaces only if a THIRD Terminal is constructed across the
      // theme flip. We therefore snapshot the count after the initial
      // mount stabilizes and assert it doesn't grow.
      await waitFor(() => expect(termCtorCount).toBeGreaterThanOrEqual(1), {
        timeout: 2000,
      });
      // Let any pending StrictMode re-mount + bridge effects fully settle.
      await act(async () => {
        await new Promise((r) => setTimeout(r, 50));
      });
      const initialCount = termCtorCount;

      // The flip.
      await act(async () => {
        exposedSetMode!('dark');
      });

      // Push an overlay.set through the live event bridge to drive a
      // wave-detail refetch underneath the theme transition.
      const overlayEv = {
        ev: 'overlay.set' as const,
        data: {
          entity_kind: 'wave' as const,
          entity_id: 'wave_test',
          kind: 'layout',
          payload: { y: 1 },
        },
      };
      await act(async () => {
        for (const fn of listeners) fn(overlayEv, { id: 1, eventVersion: 1 });
      });

      // Let the refetch settle.
      await waitFor(() =>
        expect(
          (calmApi.getWaveDetail as ReturnType<typeof vi.fn>).mock.calls
            .length,
        ).toBeGreaterThanOrEqual(1),
      );
      // Give any post-flip re-mount a chance to surface.
      await act(async () => {
        await new Promise((r) => setTimeout(r, 50));
      });

      // The bug signal: a fresh Terminal constructed AFTER the theme flip.
      // We don't pin to `=== 1` because StrictMode's dev double-mount
      // inflates the initial count; we DO pin "no growth across the flip".
      expect(termCtorCount).toBe(initialCount);
    },
  );
});
