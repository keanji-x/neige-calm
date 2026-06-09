// #177 integration test — anchor the two invariants that close the
// theme-toggle bug chain in production:
//
//   1. XtermView must NOT remount when the host theme toggles. The
//      `Terminal` constructor count in the mock below is the direct
//      signal — a fresh ctor call means a fresh xterm and a wiped
//      `pendingThemeRef` / `sendRef` pair on the React side.
//   2. The `TerminalThemeUpdate` frame must reach the daemon over
//      the live WebSocket on every toggle, not just the initial mount.
//
// We mount the closest reasonable approximation of the production tree:
//
//   <ThemeProvider>                          (real, from app/theme.tsx)
//     <QueryClientProvider>                  (real RQ client)
//       <Suspense fallback="loading">        (matches WavePage's Suspense)
//         <CodexEntry.Component card={..} /> (real codex card via registry)
//       </Suspense>
//     </QueryClientProvider>
//   </ThemeProvider>
//
// `CodexEntry.Component` internally `React.lazy()`-imports XtermView the
// same way the production card does, so the Suspense / lazy boundary is
// real. We mock the heavy bits (`@xterm/xterm`, the shared event stream)
// to keep the test deterministic; the relevant moving parts (theme
// effect → WS send → idempotency on remount) all live in app code so
// the mocks don't compromise the assertion.
//
// Why this complements the unit test: `XtermView.test.tsx` exercises
// the dispatch path in isolation, but the production bug only
// reproduced when a theme toggle was mediated by the real
// ThemeProvider → CodexCardImpl → lazy(XtermView) chain. This test
// covers that chain explicitly.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { Suspense, type ReactNode } from 'react';
import { act, render, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ThemeProvider, useTheme } from '../app/theme';
import { CodexEntry, type CodexCardData } from '../cards/builtins/codex';
import { CardInstanceProvider } from '../cards/registry';

// ---- xterm mock --------------------------------------------------------
//
// Count `new Terminal()` calls — one per XtermView mount. If the count
// climbs from 1 → 2 across the theme toggle, XtermView remounted, the
// bug reproduced, and the assertion fires.

let termCtorCount = 0;

vi.mock('@xterm/xterm', () => {
  class Terminal {
    cols = 80;
    rows = 24;
    write = vi.fn();
    writeln = vi.fn();
    clear = vi.fn();
    resize = vi.fn();
    open = vi.fn();
    loadAddon = vi.fn();
    dispose = vi.fn();
    attachCustomKeyEventHandler = vi.fn();
    options: Record<string, unknown> = {};
    parser = {
      registerOscHandler: vi.fn(() => ({ dispose: () => {} })),
    };
    onData(_cb: (d: string) => void): { dispose: () => void } {
      return { dispose: () => {} };
    }
    constructor(opts: Record<string, unknown> = {}) {
      termCtorCount += 1;
      this.options = { ...opts };
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

// ---- shared event stream mock -----------------------------------------
//
// CodexCardImpl subscribes to the shared WS event stream for
// codex.hook + overlay.set events. We don't need any of that machinery
// here — stub it out wholesale.

vi.mock('../api/events', () => ({
  sharedEventStream: vi.fn(() => ({
    addTopic: () => {},
    removeTopic: () => {},
    on: () => () => {},
  })),
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
  fireOpen: () => void;
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
  fireOpen(): void {
    this.readyState = FakeWebSocketCtor.OPEN;
    this.onopen?.({});
  }
}

// ---- harness -----------------------------------------------------------

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
}

function Wrap({ children }: { children: ReactNode }) {
  const client = makeClient();
  return (
    <ThemeProvider>
      <QueryClientProvider client={client}>
        <Suspense fallback={<div>loading</div>}>{children}</Suspense>
      </QueryClientProvider>
    </ThemeProvider>
  );
}

/** Tiny side-channel for flipping theme from inside the same tree
 *  that owns the ThemeProvider — render this component beside the
 *  card. The `ref`-equivalent it stashes on `window` would over-pollute
 *  jsdom; an exported `useTheme().setMode` call from a sibling does
 *  the job without leaking globals. */
let setMode: ((m: 'light' | 'dark' | 'system') => void) | null = null;
function ThemeCapture() {
  const ctx = useTheme();
  setMode = ctx.setMode;
  return null;
}

const codexCard: CodexCardData = {
  type: 'codex',
  id: 'card_test',
  terminalId: 'term_test',
};

function CodexUnderProvider() {
  const Codex = CodexEntry.Component;
  return (
    <CardInstanceProvider cardId={codexCard.id!} deletable card={codexCard}>
      <Codex card={codexCard} />
    </CardInstanceProvider>
  );
}

beforeEach(() => {
  termCtorCount = 0;
  wsInstances = [];
  setMode = null;
  (globalThis as { WebSocket: typeof WebSocket }).WebSocket =
    FakeWebSocketCtor as unknown as typeof WebSocket;
  (globalThis as { ResizeObserver: typeof ResizeObserver }).ResizeObserver =
    class {
      observe() {}
      disconnect() {}
      unobserve() {}
    } as unknown as typeof ResizeObserver;
  Object.defineProperty(HTMLElement.prototype, 'offsetWidth', {
    configurable: true,
    get: () => 800,
  });
  Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
    configurable: true,
    get: () => 400,
  });
});

afterEach(() => {
  wsInstances = [];
  // jsdom keeps `<html data-theme>` between tests — reset so the
  // next test doesn't inherit whatever the previous one left behind.
  try {
    if (typeof document !== 'undefined') {
      delete (document.documentElement as HTMLElement & {
        dataset: DOMStringMap;
      }).dataset.theme;
    }
  } catch {
    /* environment-specific cleanup, best-effort */
  }
});

// ---- the assertions ---------------------------------------------------

describe('#177 — theme toggle does not remount XtermView', () => {
  it('Terminal constructor stays at 1 across light → dark → light toggle', async () => {
    render(
      <Wrap>
        <ThemeCapture />
        <CodexUnderProvider />
      </Wrap>,
    );
    // XtermView is lazy-loaded; wait for the Terminal ctor to fire.
    await waitFor(() => expect(termCtorCount).toBe(1));
    expect(setMode).not.toBeNull();
    // Flip dark; wait a tick so the theme-effect runs.
    await act(async () => {
      setMode!('dark');
    });
    await new Promise((r) => setTimeout(r, 20));
    expect(termCtorCount).toBe(1);
    // Flip back to light.
    await act(async () => {
      setMode!('light');
    });
    await new Promise((r) => setTimeout(r, 20));
    expect(termCtorCount).toBe(1);
  });

  it('TerminalThemeUpdate is dispatched on every toggle (live WS)', async () => {
    render(
      <Wrap>
        <ThemeCapture />
        <CodexUnderProvider />
      </Wrap>,
    );
    await waitFor(() => expect(wsInstances.length).toBe(1));
    const ws = wsInstances[0]!;
    // Fire ws.onopen so the queued ClientHello + initial theme frame
    // both land on the wire.
    act(() => {
      ws.fireOpen();
    });
    // Initial mount → ClientHello + TerminalThemeUpdate at minimum.
    const initialThemeFrames = ws.sentFrames
      .map((s) => JSON.parse(s))
      .filter((f) => typeof f === 'object' && f !== null && 'TerminalThemeUpdate' in f);
    expect(initialThemeFrames.length).toBeGreaterThanOrEqual(1);

    // Reset wire so we observe only the toggle.
    ws.sentFrames.length = 0;
    await act(async () => {
      setMode!('dark');
    });
    // Theme-effect dispatches via the live `sendRef` (WS is OPEN now).
    await waitFor(() => {
      const themeFrames = ws.sentFrames
        .map((s) => JSON.parse(s))
        .filter((f) => 'TerminalThemeUpdate' in f);
      expect(themeFrames.length).toBeGreaterThanOrEqual(1);
    });
    // Confirm the dispatched RGB matches dark theme.
    const themeFrames = ws.sentFrames
      .map((s) => JSON.parse(s))
      .filter((f) => 'TerminalThemeUpdate' in f);
    expect(themeFrames[themeFrames.length - 1].TerminalThemeUpdate).toEqual({
      fg: [216, 219, 226],
      bg: [15, 20, 24],
    });
  });
});
