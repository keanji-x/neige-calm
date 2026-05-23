// ThemeProvider + useTheme() — the single source of truth for the app's
// light/dark theme (issue #22).
//
// What it owns
// ------------
//   * `mode`     — what the user picked: 'light' | 'dark' | 'system'.
//   * `resolved` — the effective theme after resolving 'system' against the
//                  OS `prefers-color-scheme` query. Always 'light' | 'dark'.
//   * `setMode`  — sets the user choice and persists it to localStorage.
//
// What writes `document.documentElement.dataset.theme`
// ---------------------------------------------------
// ONLY this provider, in a single useEffect keyed on `resolved`. Anywhere
// else in the codebase that needs to know the theme must call `useTheme()`
// rather than read `dataset.theme` or `prefers-color-scheme` directly.
// This is the central invariant the refactor enforces (issue #22 acceptance
// criterion 4).
//
// Persistence
// -----------
// Key: `calm.theme`. Value: one of 'light' / 'dark' / 'system'. Default is
// 'system' on first run — the spec from issue #22. We read the localStorage
// value *synchronously* during the lazy `useState` initializer so the first
// paint already has the correct `data-theme` attribute on <html> (avoiding
// a light→dark flash on reload when the user previously chose 'dark').
//
// OS listener
// -----------
// When `mode === 'system'`, we subscribe to
// `window.matchMedia('(prefers-color-scheme: dark)')` and update `resolved`
// on `change`. The listener is torn down (and re-installed) whenever `mode`
// flips. When `mode` is an explicit choice ('light' or 'dark'), no listener
// is active and `resolved` is just a direct echo of `mode`.
//
// SSR / no-window safety
// ----------------------
// The provider has to render even if `window` / `localStorage` /
// `matchMedia` are undefined (e.g. in a Vitest/jsdom corner case or a
// future SSR pass). Every access to those globals is guarded; on a
// no-window environment we default to 'system' → 'light'.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  type ReactNode,
} from 'react';
import { useState } from '../shared/state';

export type ThemeMode = 'light' | 'dark' | 'system';
export type ResolvedTheme = 'light' | 'dark';

interface ThemeContextValue {
  /** What the user picked. Persisted to localStorage. */
  mode: ThemeMode;
  /** Effective theme after resolving 'system' against the OS preference. */
  resolved: ResolvedTheme;
  /** Update the user's choice. Persisted; the DOM follows on the next effect. */
  setMode: (mode: ThemeMode) => void;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

/** localStorage key. Project namespace prefix matches `calm.*` conventions
 *  elsewhere (e.g. the React-Query persister's key). */
const STORAGE_KEY = 'calm.theme';

/** Narrow a possibly-untrusted string read from localStorage to ThemeMode. */
function parseMode(raw: string | null | undefined): ThemeMode | null {
  if (raw === 'light' || raw === 'dark' || raw === 'system') return raw;
  return null;
}

/** Read the persisted mode, defaulting to 'system'. Synchronous so the
 *  provider's lazy initializer can use it for the first render. */
function readPersistedMode(): ThemeMode {
  if (typeof window === 'undefined') return 'system';
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    return parseMode(raw) ?? 'system';
  } catch {
    // localStorage can throw under cross-origin iframes / private-mode
    // browser settings. Fall back to 'system'.
    return 'system';
  }
}

/** Return the current OS preference, defaulting to 'light' when matchMedia
 *  is unavailable or throws. */
function readSystemPreference(): ResolvedTheme {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
    return 'light';
  }
  try {
    return window.matchMedia('(prefers-color-scheme: dark)').matches
      ? 'dark'
      : 'light';
  } catch {
    return 'light';
  }
}

/** Compute the effective theme for a given mode + system snapshot. */
function resolveTheme(mode: ThemeMode, system: ResolvedTheme): ResolvedTheme {
  return mode === 'system' ? system : mode;
}

/**
 * Provider for the app-global theme. Mount once near the top of the tree —
 * see `app/providers.tsx`. Inside, every component can call `useTheme()`.
 */
export function ThemeProvider({ children }: { children: ReactNode }) {
  // Lazy initializer: runs ONCE on mount, synchronously. Doing it this way
  // means our first render already knows the persisted mode and the current
  // OS preference, so the `dataset.theme` write below lands before any
  // child re-paint.
  const [mode, setModeState] = useState<ThemeMode>(readPersistedMode);
  const [systemPref, setSystemPref] = useState<ResolvedTheme>(readSystemPreference);

  // Mirror the OS preference when `mode === 'system'`. We use a fresh
  // listener per mode change so flipping to an explicit choice tears down
  // the subscription (cheap, but more importantly: no stale `setSystemPref`
  // calls firing when the user has explicitly opted out of OS tracking).
  useEffect(() => {
    if (mode !== 'system') return;
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
      return;
    }
    let mql: MediaQueryList;
    try {
      mql = window.matchMedia('(prefers-color-scheme: dark)');
    } catch {
      return;
    }
    // Snap to whatever the OS says right now in case it changed between
    // mount and this effect running (rare but harmless).
    setSystemPref(mql.matches ? 'dark' : 'light');
    const onChange = (e: MediaQueryListEvent) => {
      setSystemPref(e.matches ? 'dark' : 'light');
    };
    // `addEventListener` is the modern API; the legacy `addListener` is
    // still around in some embedded engines but we don't care about those
    // for this app's browser support matrix.
    mql.addEventListener('change', onChange);
    return () => {
      mql.removeEventListener('change', onChange);
    };
  }, [mode]);

  const resolved: ResolvedTheme = useMemo(
    () => resolveTheme(mode, systemPref),
    [mode, systemPref],
  );

  // The ONLY place in the app that writes `document.documentElement.dataset.theme`.
  // Anywhere else needs to call `useTheme()` and read `resolved`.
  useEffect(() => {
    if (typeof document === 'undefined') return;
    document.documentElement.dataset.theme = resolved;
  }, [resolved]);

  const setMode = useCallback((next: ThemeMode) => {
    setModeState(next);
    if (typeof window === 'undefined') return;
    try {
      window.localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Same private-mode / cross-origin guard as the read path. The state
      // update still wins; we just won't survive a reload.
    }
  }, []);

  const value = useMemo<ThemeContextValue>(
    () => ({ mode, resolved, setMode }),
    [mode, resolved, setMode],
  );

  // #177 — Playwright instrumentation. Gated on `?testMounts=1` so
  // production users never see the global. Exposes a driver the e2e
  // regression spec (`web/e2e/a11y-177-theme-toggle-no-remount.spec.ts`)
  // uses to flip the theme WITHOUT navigating to the Settings page —
  // navigation would unmount any wave-page XtermView under test and
  // defeat the whole observation.
  useEffect(() => {
    if (typeof window === 'undefined') return;
    const url = new URL(window.location.href);
    if (url.searchParams.get('testMounts') !== '1') return;
    const w = window as unknown as { __calmSetTheme?: (m: ThemeMode) => void };
    w.__calmSetTheme = setMode;
    return () => {
      if (w.__calmSetTheme === setMode) delete w.__calmSetTheme;
    };
  }, [setMode]);

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

/**
 * Read the app's current theme. Must be called inside `<ThemeProvider>`.
 *
 * Returns `{ mode, resolved, setMode }`. Components that only care about
 * applying styles should read `resolved` (the effective light/dark);
 * components that surface a UI control (the Settings page's Appearance
 * radio) may also read `mode`.
 */
export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) {
    throw new Error(
      'useTheme() called outside of <ThemeProvider>. Wrap your tree in <ThemeProvider> ' +
        '(see web/src/app/providers.tsx).',
    );
  }
  return ctx;
}
