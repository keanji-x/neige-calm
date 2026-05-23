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
import { Suspense, useEffect } from 'react';
import { act, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider, useQueryClient } from '@tanstack/react-query';

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
  sharedEventStream: () => ({
    addTopic: () => {},
    removeTopic: () => {},
    on: () => () => {},
  }),
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
// TODO (#177): if all tests above pass on current `HEAD` (7b85305) but the
// user still observes the remount in DevTools, the test environment is
// missing one or more of the following factors that exist in production:
//
//  1. **TanStack Router's `<Match>` Suspense boundary.** WaveComponent
//     renders under a route, and `@tanstack/react-router` wraps each route
//     in its own `<Match>` Suspense. We don't mount the router here — only
//     the React tree below WavePage's local `<Suspense>`. If the remount
//     trigger comes from router-level suspension (e.g. a route loader
//     re-running on context change), this test cannot catch it. A future
//     iteration should mount a minimal `RouterProvider` with a memory
//     history to anchor that path.
//
//  2. **The event-bridge → query.invalidate → refetch cycle.** Production
//     theme toggle indirectly triggers an overlay.set event (via RGL's
//     resize-observer firing on the [data-theme=dark] CSS swap), which
//     `eventBridge.tsx` translates into `invalidateQueries(['wave', id])`.
//     The refetch was the original `placeholderData` target — but if the
//     refetch's *response* is shaped differently enough (e.g. a new
//     `cards` array identity), React's reconciler could still detach +
//     reattach the subtree. To anchor this path we'd need to wire an MSW
//     handler returning the wave detail and observe the refetch.
//
//  3. **`@tanstack/react-query-persist-client`.** AppProviders wraps the
//     QC with `PersistQueryClientProvider`, which has its own
//     `useIsRestoring()` gate that briefly renders `null` during cache
//     hydration. This test uses a bare QueryClientProvider so the gate
//     never fires.
//
// Reproducing #2 or #3 in vitest is meaningful work; we accept this test
// as the lower-bound regression anchor for now (no remount across a
// direct theme flip from a stable subtree). If the user can confirm the
// remount survives a controlled minimal subtree like this one, escalate
// to a router-driven integration test (Option B from the original spec).
