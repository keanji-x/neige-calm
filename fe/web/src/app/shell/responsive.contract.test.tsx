// @vitest-environment jsdom
import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { AppShell } from './public.tsx';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { NEUTRAL_ACTIVITY } from '../../../../core/domain/wave.ts';

vi.mock('@tanstack/react-router', () => ({ Outlet: () => <div>route</div> }));
const COVE = { id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0 };
const WAVE = {
  id: 'w1', coveId: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0, ...NEUTRAL_ACTIVITY,
};

vi.mock('../providers/queries.ts', () => ({
  useWorkspace: () => ({
    coves: [COVE], waves: [WAVE], wavesByCove: new Map([['c1', [WAVE]]]), waveErrorsByCove: new Map(), wavesLoadingByCove: new Map(),
    covesError: null, overlaysError: null, covesLoading: false, overlaysLoading: false,
    retryCoves: vi.fn(), retryOverlays: vi.fn(), retryWaves: vi.fn(),
  }),
  useCoveMutations: () => ({ create: vi.fn(), remove: vi.fn() }),
  useWaveMutations: () => ({ setPinned: vi.fn(), create: vi.fn(), remove: vi.fn() }),
  ApiError: class ApiError extends Error {},
}));
vi.mock('../router/navigation.ts', () => ({
  useCurrentPath: () => '/',
  useGo: () => vi.fn(),
  useGoSameWave: () => vi.fn(),
  routeParamFromPath: () => undefined,
}));
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
    // #1191 3ec80a6b — dock 是幂等的目的地，不是 toggle：再次点击 Pages 仍停在 Pages。
    // 这不是漏了关闭断言，别把它「修」回 toggle。关闭走 Escape（见下）或 dock 的其它目的地。
    // 这里是该语义在真实 AppShell 上的唯一守卫：mobile.browser.test.tsx 用的是自建替身。
    fireEvent.click(pages);
    expect(screen.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
    expect(pages.getAttribute('aria-expanded')).toBe('true');

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

    /*
     * The dock yields to a secondary page. That used to be published as a
     * `window` event; since #1191 §2.1 it is derived, so the only way to reach
     * it here is to drive the state it is derived from — drilling the Coves
     * sheet into a cove. `useCurrentPath` is mocked to `/`, so the wave-route
     * half of the OR is out of play and this is the cove half on its own.
     */
    const dock = document.querySelector('nav[aria-label="Primary"]');
    expect(dock?.getAttribute('aria-hidden')).toBeNull();
    fireEvent.click(opener);
    fireEvent.click(screen.getByRole('button', { name: /Product/ }));
    expect(dock?.getAttribute('aria-hidden')).toBe('true');
    expect(dock?.hasAttribute('inert')).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Back to Coves' }));
    expect(dock?.getAttribute('aria-hidden')).toBeNull();
  });
});
