// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { AppShell } from './public.tsx';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';

/* Keep real router/history state for shell-owned surfaces while isolating
 * navigation effects. An empty useRouter result hid required history fields. */
vi.mock('@tanstack/react-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tanstack/react-router')>();
  const router = actual.createRouter({
    routeTree: actual.createRootRoute(),
    history: actual.createMemoryHistory({ initialEntries: ['/'] }),
  });
  return {
    ...actual,
    Outlet: () => <div>route</div>,
    useNavigate: () => vi.fn(),
    useRouter: () => router,
    useRouterState: ({ select }: { select: (state: typeof router.state) => unknown }) => select(router.state),
  };
});
const AREA: Area = {
  id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user',
  defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0,
};
const TRACK: Track = {
  id: 'w1', areaId: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0, ...NEUTRAL_ACTIVITY,
};
// Read when `useWorkspace` is called, so a case may hand the shell its own rows.
let workspaceTracks: readonly Track[] = [TRACK];

vi.mock('../providers/queries.ts', () => ({
  useWorkspace: () => ({
    areas: [AREA], tracks: workspaceTracks, tracksByArea: new Map([['c1', workspaceTracks]]), trackErrorsByArea: new Map(), tracksLoadingByArea: new Map(),
    areasError: null, overlaysError: null, areasLoading: false, overlaysLoading: false,
    retryAreas: vi.fn(), retryOverlays: vi.fn(), retryTracks: vi.fn(),
  }),
  useAreaMutations: () => ({ create: vi.fn(), update: vi.fn(), remove: vi.fn() }),
  useTrackMutations: () => ({ setPinned: vi.fn(), create: vi.fn(), remove: vi.fn() }),
  // #1209 — the dialog's template read. Blank-only is a working state, so the
  // rail contract needs nothing more than the degraded shape here.
  useTrackTemplates: () => ({ templates: [], error: null, loaded: true, refetch: vi.fn() }),
  ApiError: class ApiError extends Error {},
}));
/*
 * A *partial* mock: the hooks are stubbed, but `pathFor` — which the dock's
 * selection rule reads the route table from (#1191 §3.3) — stays the real one.
 * Re-declaring it here would put a second copy of the route table in a test.
 */
vi.mock('../router/navigation.ts', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../router/navigation.ts')>()),
  useCurrentPath: () => '/',
  useGo: () => vi.fn(),
  useGoSameTrack: () => vi.fn(),
  routeParamFromPath: () => undefined,
}));
vi.mock('./sidebar.tsx', () => ({ Sidebar: ({ collapsed, onToggleCollapsed }: {
  collapsed: boolean; onToggleCollapsed: () => void;
}) => <button type="button" aria-expanded={!collapsed} onClick={onToggleCollapsed}>{collapsed ? 'Expand' : 'Collapse'}</button> }));

afterEach(() => { cleanup(); vi.unstubAllGlobals(); workspaceTracks = [TRACK]; });

function compactViewport() {
  const listeners = new Set<() => void>();
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: true, media: '', onchange: null,
    addEventListener: (_: string, listener: () => void) => listeners.add(listener),
    removeEventListener: (_: string, listener: () => void) => listeners.delete(listener),
    addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
  })));
}

function memoryStorage() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

describe('compact navigation interaction contracts', () => {
  it('opens the workspace as a modal side page and Escape returns to content', () => {
    compactViewport();
    const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
    render(<AppShell transport={{} as never} unauthorized={unauthorized} onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()} />);
    expect(screen.queryByRole('navigation', { name: 'Primary' })).toBeNull();
    const opener = screen.getByRole('button', { name: 'Open areas' });
    fireEvent.click(opener);
    expect(screen.getByRole('dialog', { name: 'Tracks and settings' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Settings' })).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Responsive mobile UI/ })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Product' }));
    expect(screen.getByRole('button', { name: /Responsive mobile UI/ })).toBeTruthy();
    expect(document.querySelector('main')?.hasAttribute('inert')).toBe(true);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: 'Tracks and settings' })).toBeNull();
    const area = screen.getByRole('button', { name: 'Switch area, Product' });
    fireEvent.click(area);
    expect(screen.getByRole('menuitem', { name: 'Product' })).toBeTruthy();
  });

  /*
   * #1722 §5.2 — the shell hands the phone's Track list the rail's receipt,
   * key for key: a completion the kernel recorded after this reader's last
   * look is unread on the phone, and the same `markRead` that the track page
   * makes clears it. `MobileTracks` alone cannot show this (its receipt is a
   * prop); only the shell decides which receipt that prop is.
   */
  it('hands the Track list the rail’s read receipt, so a newer completion is unread until it is read', () => {
    compactViewport();
    workspaceTracks = [{ ...TRACK, activityAt: 150 }];
    // The scope was entered at server time 100: that is the baseline receipt.
    const preferences = createUiPreferences(memoryStorage());
    preferences.setReadScope('db1', 100);
    const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
    render(<UiPreferencesProvider preferences={preferences}>
      <AppShell transport={{} as never} unauthorized={unauthorized} onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()} />
    </UiPreferencesProvider>);
    fireEvent.click(screen.getByRole('button', { name: 'Open areas' }));
    fireEvent.click(screen.getByRole('button', { name: 'Product' }));
    const marker = () => screen.getByRole('button', { name: /^Responsive mobile UI/ })
      .querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity') ?? null;
    expect(marker()).toBe('unread');
    expect(screen.getByRole('button', { name: 'Responsive mobile UI' })).toBeTruthy();

    act(() => preferences.markRead('track', 'w1', 150));
    expect(marker()).toBeNull();
  });
});
