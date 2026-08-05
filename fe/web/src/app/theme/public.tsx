import { createContext, useCallback, useContext, useEffect, useMemo, type ReactNode } from 'react';
import { THEME_KEY } from '../../../../core/keys/storage.ts';
import { useState } from '../../ui/state/public.ts';

export const THEME_MODES = Object.freeze(['light', 'dark', 'system'] as const);
export type ThemeMode = (typeof THEME_MODES)[number];
export type ResolvedTheme = Exclude<ThemeMode, 'system'>;
export type ThemeContextValue = Readonly<{ mode: ThemeMode; resolved: ResolvedTheme; setMode: (mode: ThemeMode) => void }>;

declare global { interface Window { __calmSetTheme?: (mode: ThemeMode) => void } }

const ThemeContext = createContext<ThemeContextValue | null>(null);

export function parseThemeMode(raw: unknown): ThemeMode | null {
  return raw === 'light' || raw === 'dark' || raw === 'system' ? raw : null;
}

export type ThemeStorage = Pick<Storage, 'getItem' | 'setItem'>;

function readPersistedMode(storage: ThemeStorage | undefined): ThemeMode {
  if (!storage) return 'system';
  try { return parseThemeMode(storage.getItem(THEME_KEY)) ?? 'system'; } catch { return 'system'; }
}

function readSystemPreference(): ResolvedTheme {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return 'light';
  try { return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'; } catch { return 'light'; }
}

export function ThemeProvider({ children, storage }: { children: ReactNode; storage?: ThemeStorage }) {
  const [mode, setModeState] = useState<ThemeMode>(() => readPersistedMode(storage));
  const [systemPreference, setSystemPreference] = useState<ResolvedTheme>(readSystemPreference);

  useEffect(() => {
    if (mode !== 'system' || typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
    let media: MediaQueryList;
    try { media = window.matchMedia('(prefers-color-scheme: dark)'); } catch { return; }
    setSystemPreference(media.matches ? 'dark' : 'light');
    const change = (event: MediaQueryListEvent) => setSystemPreference(event.matches ? 'dark' : 'light');
    media.addEventListener('change', change);
    return () => media.removeEventListener('change', change);
  }, [mode]);

  const resolved = mode === 'system' ? systemPreference : mode;
  useEffect(() => { if (typeof document !== 'undefined') document.documentElement.dataset.theme = resolved; }, [resolved]);

  const setMode = useCallback((next: ThemeMode) => {
    setModeState(next);
    if (!storage) return;
    try { storage.setItem(THEME_KEY, next); } catch { /* Persistence is best-effort. */ }
  }, [storage]);

  useEffect(() => {
    if (typeof window === 'undefined') return;
    const url = new URL(window.location.href);
    if (url.searchParams.get('testMounts') !== '1') return;
    window.__calmSetTheme = setMode;
    return () => { if (window.__calmSetTheme === setMode) delete window.__calmSetTheme; };
  }, [setMode]);

  const value = useMemo<ThemeContextValue>(() => ({ mode, resolved, setMode }), [mode, resolved, setMode]);
  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme(): ThemeContextValue {
  const value = useContext(ThemeContext);
  if (!value) throw new Error('useTheme() requires <ThemeProvider>; mount it from web/src/app/providers/public.tsx.');
  return value;
}
