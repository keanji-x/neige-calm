// @vitest-environment jsdom
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { AppShell } from './public.tsx';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { setMobileSecondaryOpen } from '../../ui/mobile-page/public.ts';

vi.mock('@tanstack/react-router', () => ({ Outlet: () => <div>route</div> }));
vi.mock('../providers/queries.ts', () => ({
  useWorkspace: () => ({
    coves: [], waves: [], wavesByCove: new Map(), waveErrorsByCove: new Map(), wavesLoadingByCove: new Map(),
    covesError: null, overlaysError: null, covesLoading: false, overlaysLoading: false,
    retryCoves: vi.fn(), retryOverlays: vi.fn(), retryWaves: vi.fn(),
  }),
  useCoveMutations: () => ({ create: vi.fn(), remove: vi.fn() }),
  useWaveMutations: () => ({ setPinned: vi.fn(), create: vi.fn(), remove: vi.fn() }),
  ApiError: class ApiError extends Error {},
}));
vi.mock('../router/navigation.ts', () => ({ useCurrentPath: () => '/', useGo: () => vi.fn() }));
vi.mock('./sidebar.tsx', () => ({ Sidebar: ({ collapsed, onToggleCollapsed }: {
  collapsed: boolean; onToggleCollapsed: () => void;
}) => <button type="button" aria-expanded={!collapsed} onClick={onToggleCollapsed}>{collapsed ? 'Expand' : 'Collapse'}</button> }));

afterEach(() => vi.unstubAllGlobals());

describe('compact navigation interaction contracts', () => {
  it('opens the workspace as a modal side page and Escape returns to content', () => {
    const listeners = new Set<() => void>();
    vi.stubGlobal('matchMedia', vi.fn(() => ({
      matches: true, media: '', onchange: null,
      addEventListener: (_: string, listener: () => void) => listeners.add(listener),
      removeEventListener: (_: string, listener: () => void) => listeners.delete(listener),
      addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
    })));
    const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
    render(<AppShell transport={{} as never} unauthorized={unauthorized} onOpenSettings={vi.fn()} onSignOut={vi.fn()} />);
    const pages = screen.getByRole('button', { name: 'Pages' });
    expect(pages.getAttribute('aria-expanded')).toBe('false');
    fireEvent.click(pages);
    expect(screen.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
    expect(pages.getAttribute('aria-expanded')).toBe('true');
    fireEvent.click(pages);
    expect(screen.queryByRole('dialog', { name: 'Pages' })).toBeNull();

    const opener = screen.getByRole('button', { name: 'Coves' });
    expect(opener.getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByRole('dialog', { name: 'Coves' })).toBeNull();

    fireEvent.click(opener);
    expect(screen.getByRole('dialog', { name: 'Coves' })).toBeTruthy();
    expect(opener.getAttribute('aria-expanded')).toBe('true');
    expect(document.querySelector('main')?.hasAttribute('inert')).toBe(true);

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: 'Coves' })).toBeNull();
    expect(opener.getAttribute('aria-expanded')).toBe('false');

    const dock = document.querySelector('nav[aria-label="Primary"]');
    act(() => setMobileSecondaryOpen(true));
    expect(dock?.getAttribute('aria-hidden')).toBe('true');
    expect(dock?.hasAttribute('inert')).toBe(true);
    act(() => setMobileSecondaryOpen(false));
    expect(dock?.getAttribute('aria-hidden')).toBeNull();
  });
});
