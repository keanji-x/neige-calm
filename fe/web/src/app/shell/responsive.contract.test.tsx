// @vitest-environment jsdom
import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { AppShell } from './public.tsx';

vi.mock('@tanstack/react-router', () => ({ Outlet: () => <div>route</div> }));
vi.mock('../providers/queries.ts', () => ({
  useWorkspace: () => ({
    coves: [], waves: [], wavesByCove: new Map(), waveErrorsByCove: new Map(), wavesLoadingByCove: new Map(),
    covesError: null, overlaysError: null, covesLoading: false, overlaysLoading: false,
    retryCoves: vi.fn(), retryOverlays: vi.fn(), retryWaves: vi.fn(),
  }),
  useCoveMutations: () => ({ create: vi.fn(), remove: vi.fn() }),
  useWaveMutations: () => ({ setPinned: vi.fn(), remove: vi.fn() }),
}));
vi.mock('../router/navigation.ts', () => ({ useCurrentPath: () => '/', useGo: () => vi.fn() }));
vi.mock('./sidebar.tsx', () => ({ Sidebar: ({ collapsed, onToggleCollapsed }: {
  collapsed: boolean; onToggleCollapsed: () => void;
}) => <button type="button" aria-expanded={!collapsed} onClick={onToggleCollapsed}>{collapsed ? 'Expand' : 'Collapse'}</button> }));

afterEach(() => vi.unstubAllGlobals());

describe('narrow rail interaction contracts', () => {
  it('follows matchMedia until a narrow-screen Expand explicitly wins', () => {
    const listeners = new Set<() => void>();
    vi.stubGlobal('matchMedia', vi.fn(() => ({
      matches: true, media: '', onchange: null,
      addEventListener: (_: string, listener: () => void) => listeners.add(listener),
      removeEventListener: (_: string, listener: () => void) => listeners.delete(listener),
      addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
    })));
    const { container } = render(<AppShell transport={{} as never} onOpenSettings={vi.fn()} onSignOut={vi.fn()} />);
    const toggle = screen.getByRole('button', { name: 'Expand' });
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(container.firstElementChild?.className).toContain('shellCollapsed');
    fireEvent.click(toggle);
    expect(screen.getByRole('button', { name: 'Collapse' }).getAttribute('aria-expanded')).toBe('true');
    expect(container.firstElementChild?.className).toContain('shellExpanded');
  });
});
