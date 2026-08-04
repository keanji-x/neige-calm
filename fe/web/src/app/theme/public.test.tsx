// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { THEME_KEY } from '../../../../core/keys/storage.ts';
import { ThemeProvider, useTheme, type ThemeMode } from './public.tsx';

function memoryStorage() { const values = new Map<string, string>(); return { values, getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } }; }

function Probe() {
  const theme = useTheme();
  return <button type="button" onClick={() => theme.setMode(theme.mode === 'dark' ? 'light' : 'dark')}>{theme.mode}:{theme.resolved}</button>;
}

function createMatchMedia(initial: boolean) {
  let matches = initial;
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  const mql = { get matches() { return matches; }, media: '(prefers-color-scheme: dark)', onchange: null,
    addEventListener: (_: string, fn: (event: MediaQueryListEvent) => void) => listeners.add(fn),
    removeEventListener: (_: string, fn: (event: MediaQueryListEvent) => void) => listeners.delete(fn),
    addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: () => true } as MediaQueryList;
  return { matchMedia: vi.fn(() => mql), change(next: boolean) { matches = next; listeners.forEach((fn) => fn({ matches: next } as MediaQueryListEvent)); }, listeners };
}

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; history.replaceState(null, '', '/'); vi.restoreAllMocks(); });

describe('ThemeProvider behavior', () => {
  it('INV-APP-072 synchronously initializes persisted mode and mirrors resolved theme', () => {
    const storage = memoryStorage(); storage.setItem(THEME_KEY, 'dark');
    render(<ThemeProvider storage={storage}><Probe /></ThemeProvider>);
    expect(screen.getByRole('button').textContent).toBe('dark:dark');
    expect(document.documentElement.dataset.theme).toBe('dark');
  });

  it('INV-APP-073 accepts valid persistence, rejects invalid values, and survives storage failures', () => {
    const storage = memoryStorage(); storage.setItem(THEME_KEY, 'sepia');
    const media = createMatchMedia(false); vi.stubGlobal('matchMedia', media.matchMedia);
    render(<ThemeProvider storage={storage}><Probe /></ThemeProvider>);
    expect(screen.getByRole('button').textContent).toBe('system:light');
    storage.setItem = () => { throw new Error('blocked'); };
    act(() => screen.getByRole('button').click());
    expect(screen.getByRole('button').textContent).toBe('dark:dark');
  });

  it('CAP-APP-074 INV-APP-075 tracks system immediately and unsubscribes in explicit mode', () => {
    const media = createMatchMedia(true); vi.stubGlobal('matchMedia', media.matchMedia);
    render(<ThemeProvider storage={memoryStorage()}><Probe /></ThemeProvider>);
    expect(screen.getByRole('button').textContent).toBe('system:dark');
    expect(media.listeners.size).toBe(1);
    act(() => screen.getByRole('button').click());
    expect(media.listeners.size).toBe(0);
    act(() => media.change(false));
    expect(screen.getByRole('button').textContent).toBe('dark:dark');
  });

  it('INV-APP-078 fails descriptively outside the provider', () => {
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    expect(() => render(<Probe />)).toThrow(/ThemeProvider.*app\/providers\.tsx/u);
  });

  it('CAP-APP-076 only owns and conditionally removes its test driver', () => {
    history.replaceState(null, '', '/?testMounts=1');
    const { unmount } = render(<ThemeProvider><Probe /></ThemeProvider>);
    const driver = window.__calmSetTheme;
    expect(driver).toBeTypeOf('function');
    const successor = vi.fn<(mode: ThemeMode) => void>(); window.__calmSetTheme = successor;
    unmount();
    expect(window.__calmSetTheme).toBe(successor);
    delete window.__calmSetTheme;
    history.replaceState(null, '', '/');
    render(<ThemeProvider><Probe /></ThemeProvider>);
    expect(window.__calmSetTheme).toBeUndefined();
  });
});
