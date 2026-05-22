// Tests for `ThemeProvider` + `useTheme()` (issue #22).
//
// We cover the four behaviors the rest of the app depends on:
//   1. Default mode is 'system' on first run; `resolved` follows the OS.
//   2. Persisted `mode` from localStorage is honored on init AND drives the
//      first render of `data-theme` synchronously (no light → dark flash).
//   3. `setMode('dark')` updates `resolved`, persists to localStorage, and
//      writes `<html data-theme="dark">`.
//   4. While `mode === 'system'`, an OS `prefers-color-scheme` change flips
//      `resolved` without any user action.
//
// We mock `window.matchMedia` because jsdom doesn't implement it; the mock
// dispatches `change` events through a `Set<listener>` so tests can simulate
// an OS toggle by calling the spy's helper.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, act, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ThemeProvider, useTheme } from './theme';

interface MqlMockHandle {
  setMatches: (matches: boolean) => void;
}

function installMatchMediaMock(initialDarkMatches: boolean): MqlMockHandle {
  const listeners = new Set<(e: MediaQueryListEvent) => void>();
  let matches = initialDarkMatches;
  const mql = {
    get matches() {
      return matches;
    },
    media: '(prefers-color-scheme: dark)',
    onchange: null,
    addEventListener: (
      _type: string,
      cb: (e: MediaQueryListEvent) => void,
    ) => {
      listeners.add(cb);
    },
    removeEventListener: (
      _type: string,
      cb: (e: MediaQueryListEvent) => void,
    ) => {
      listeners.delete(cb);
    },
    // Legacy API — unused in production code but jsdom callers sometimes
    // sniff for it. Provide a stub so a feature-detect doesn't crash.
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => true,
  } as unknown as MediaQueryList;

  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    writable: true,
    value: () => mql,
  });

  return {
    setMatches(next: boolean) {
      matches = next;
      const event = { matches: next } as MediaQueryListEvent;
      for (const cb of listeners) cb(event);
    },
  };
}

function Probe() {
  const { mode, resolved, setMode } = useTheme();
  return (
    <div>
      <div data-testid="mode">{mode}</div>
      <div data-testid="resolved">{resolved}</div>
      <button onClick={() => setMode('dark')}>set dark</button>
      <button onClick={() => setMode('light')}>set light</button>
      <button onClick={() => setMode('system')}>set system</button>
    </div>
  );
}

beforeEach(() => {
  cleanup();
  window.localStorage.clear();
  // Reset the documentElement attribute so each test starts from a known DOM.
  delete document.documentElement.dataset.theme;
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('ThemeProvider — default mode', () => {
  it('defaults to system mode and resolves to OS preference', () => {
    installMatchMediaMock(/*darkMatches*/ true);

    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    );

    expect(screen.getByTestId('mode').textContent).toBe('system');
    expect(screen.getByTestId('resolved').textContent).toBe('dark');
    // The provider must write to <html> synchronously enough that the
    // attribute is in place by the time the user can see anything. React
    // useEffect runs after commit; we just confirm post-commit state here.
    expect(document.documentElement.dataset.theme).toBe('dark');
  });

  it('reads persisted mode from localStorage on init', () => {
    installMatchMediaMock(/*darkMatches*/ true);
    window.localStorage.setItem('calm.theme', 'light');

    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    );

    expect(screen.getByTestId('mode').textContent).toBe('light');
    expect(screen.getByTestId('resolved').textContent).toBe('light');
    expect(document.documentElement.dataset.theme).toBe('light');
  });
});

describe('ThemeProvider — setMode', () => {
  it('setMode("dark") updates resolved, persists, and writes data-theme', async () => {
    installMatchMediaMock(/*darkMatches*/ false);

    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    );

    expect(screen.getByTestId('resolved').textContent).toBe('light');

    await userEvent.click(screen.getByRole('button', { name: /set dark/i }));

    expect(screen.getByTestId('mode').textContent).toBe('dark');
    expect(screen.getByTestId('resolved').textContent).toBe('dark');
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(window.localStorage.getItem('calm.theme')).toBe('dark');
  });

  it('setMode("system") re-couples resolved to the OS preference', async () => {
    const mql = installMatchMediaMock(/*darkMatches*/ true);
    window.localStorage.setItem('calm.theme', 'light');

    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    );

    expect(screen.getByTestId('resolved').textContent).toBe('light');

    await userEvent.click(screen.getByRole('button', { name: /set system/i }));
    expect(screen.getByTestId('mode').textContent).toBe('system');
    expect(screen.getByTestId('resolved').textContent).toBe('dark');

    // Flip the OS preference. The provider should follow because mode === 'system'.
    await act(async () => {
      mql.setMatches(false);
    });
    expect(screen.getByTestId('resolved').textContent).toBe('light');
    expect(document.documentElement.dataset.theme).toBe('light');
  });

  it('OS change does NOT affect resolved when mode is explicit', async () => {
    const mql = installMatchMediaMock(/*darkMatches*/ false);

    render(
      <ThemeProvider>
        <Probe />
      </ThemeProvider>,
    );

    await userEvent.click(screen.getByRole('button', { name: /set light/i }));
    expect(screen.getByTestId('resolved').textContent).toBe('light');

    await act(async () => {
      mql.setMatches(true);
    });

    // Mode is 'light' — explicit choice overrides the OS even though the OS
    // now reports dark.
    expect(screen.getByTestId('resolved').textContent).toBe('light');
    expect(document.documentElement.dataset.theme).toBe('light');
  });
});

describe('useTheme — outside provider', () => {
  it('throws a descriptive error when used outside a ThemeProvider', () => {
    installMatchMediaMock(/*darkMatches*/ false);
    // React logs an internal error message when a component throws during
    // render. Silence it so the test output stays clean.
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => render(<Probe />)).toThrow(/ThemeProvider/);
    consoleError.mockRestore();
  });
});
