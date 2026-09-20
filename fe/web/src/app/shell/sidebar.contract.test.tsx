// @vitest-environment jsdom
// Invariants for the workspace rail.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function memoryStorage() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

function area(overrides: Partial<Area> = {}): Area {
  return {
    id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0, ...overrides,
  };
}

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Task', sort: 1, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

function renderSidebar(props: Partial<Parameters<typeof Sidebar>[0]> = {}) {
  const tracks = props.tracks ?? [];
  return render(
    <ThemeProvider storage={memoryStorage()}>
      <Sidebar
        areas={props.areas ?? [area()]}
        tracksByArea={props.tracksByArea ?? new Map([['c1', tracks]])}
        tracks={tracks}
        currentPath={props.currentPath ?? '/'}
        onGo={props.onGo ?? vi.fn()}
        onRequestCreateArea={props.onRequestCreateArea ?? vi.fn()}
        onRequestEditArea={props.onRequestEditArea ?? vi.fn()}
        onDeleteArea={props.onDeleteArea ?? vi.fn()}
        onNewTrack={props.onNewTrack ?? vi.fn()}
        onSetPinned={props.onSetPinned ?? vi.fn()}
        onDeleteTrack={props.onDeleteTrack ?? vi.fn()}
        collapsed={props.collapsed ?? false}
        onToggleCollapsed={props.onToggleCollapsed ?? vi.fn()}
        onOpenSettings={props.onOpenSettings ?? vi.fn()}
        onOpenPlugins={props.onOpenPlugins ?? vi.fn()}
        onSignOut={props.onSignOut ?? vi.fn()}
      />
    </ThemeProvider>,
  );
}

describe('INV-SIDEBAR-007 three sections, and pinning is not relocation', () => {
  const pinnedAndBlocked = track({ id: 'both', title: 'Both', lifecycle: 'blocked', attention: 'input', pinnedAt: 10 });

  it('renders Waiting on you, then Pinned, then Areas', () => {
    renderSidebar({ tracks: [pinnedAndBlocked] });
    const headings = screen.getAllByRole('heading').map((node) => node.textContent);
    expect(headings).toEqual(['Waiting on you', 'Pinned', 'Areas']);
  });

  it('keeps a pinned track in its area list as well as in the Pinned section', () => {
    renderSidebar({ tracks: [track({ id: 'p', title: 'Pinned task', pinnedAt: 10 })] });
    // One row under Pinned (carries the area name) and one inside the area list.
    expect(screen.getAllByRole('button', { name: /^Track Pinned task/ })).toHaveLength(2);
  });

  it('surfaces a pinned attention track in Waiting on you too — three rows, not one', () => {
    renderSidebar({ tracks: [pinnedAndBlocked] });
    expect(screen.getAllByRole('button', { name: /^Track Both/ })).toHaveLength(3);
  });

  it('drops a section entirely when it is empty rather than reordering the rest', () => {
    renderSidebar({ tracks: [track()] });
    expect(screen.getAllByRole('heading').map((node) => node.textContent)).toEqual(['Areas']);
  });
});

describe('INV-SIDEBAR-012 the pin button is always in the accessibility tree', () => {
  // The visual reveal is CSS jsdom does not apply; what is provable here is that
  // the control exists and is reachable in both states.
  it('exposes a pressed-state pin control for pinned and unpinned tracks alike', () => {
    renderSidebar({
      tracks: [track({ id: 'u', title: 'Loose' }), track({ id: 'p', title: 'Stuck', pinnedAt: 10 })],
    });
    const unpinned = screen.getByRole('button', { name: 'Pin Loose' });
    expect(unpinned.getAttribute('aria-pressed')).toBe('false');
    // The pinned track renders twice (Pinned section + area list); both carry it.
    const pinned = screen.getAllByRole('button', { name: 'Unpin Stuck' });
    expect(pinned).toHaveLength(2);
    for (const node of pinned) expect(node.getAttribute('aria-pressed')).toBe('true');
  });
});

describe('INV-SIDEBAR-013 every area row carries a permanent New track control', () => {
  const areas = [area(), area({ id: 'c2', name: 'Reading', sort: 2 })];
  const tracksByArea = new Map([['c1', []], ['c2', []]]);

  /* One control per area, so `"New track"` alone would be N identically-named
   * controls; the tooltip may not stand in for the accessible name. */
  it('names each one for its own area and still carries a tooltip', () => {
    renderSidebar({ areas, tracksByArea });
    for (const areaName of ['Work', 'Reading']) {
      const button = screen.getByRole('button', { name: `New track in ${areaName}` });
      expect(button.getAttribute('title')).toBe('New track');
      expect(button.tagName).toBe('BUTTON');
    }
    expect(screen.queryByRole('button', { name: 'New track' })).toBeNull();
  });

  /* jsdom cannot read positioned geometry, so this pins their separate styling hooks. */
  it('keeps New track and Area actions in separate permanent control slots', () => {
    renderSidebar({ areas, tracksByArea });
    const create = screen.getByRole('button', { name: 'New track in Work' });
    const actions = screen.getByRole('button', { name: 'Area actions for Work' });
    expect(create.className).not.toBe(actions.className);
    expect(create.className.split(/\s+/).some((token) => actions.className.split(/\s+/).includes(token)))
      .toBe(false);
  });

  /** The collapsed rail has room for one glyph per area, and that glyph is the area. */
  it('offers no New track control in the collapsed icon strip', () => {
    renderSidebar({ areas, tracksByArea, collapsed: true });
    expect(screen.queryByRole('button', { name: /^New track/ })).toBeNull();
  });
});

describe('E2E-INV-SHELL-003 the kernel system area never reaches the rail', () => {
  it('renders zero area rows for a workspace whose only area is a system area', () => {
    renderSidebar({
      areas: [area({ id: 'sys', name: 'System', kind: 'system' })],
      tracks: [track({ id: 'k', areaId: 'sys', title: 'Kernel' })],
      tracksByArea: new Map([['sys', [track({ id: 'k', areaId: 'sys', title: 'Kernel' })]]]),
    });
    expect(screen.queryByRole('button', { name: /^System/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /^Track Kernel/ })).toBeNull();
    // When a region's emptiness has exactly one remedy, that remedy's own interface
    // renders where the content would have been.
    expect(screen.queryByText(/no areas/i)).toBeNull();
    expect(screen.getByRole('button', { name: 'Create your first area' })).toBeTruthy();
  });
});

describe('INV-A11Y-058 there is intentionally no skip link', () => {
  it('exposes no skip-to-content affordance', () => {
    const { container } = renderSidebar({ tracks: [track()] });
    expect(screen.queryByRole('link', { name: /skip/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /skip/i })).toBeNull();
    expect(container.textContent).not.toMatch(/skip to/i);
  });
});

describe('INV-A11Y-061 navigation shape', () => {
  it('uses buttons for every navigation row and emits no native links', () => {
    const onGo = vi.fn();
    const { container } = renderSidebar({ tracks: [track({ id: 'w9', title: 'Row' })], onGo });
    expect(container.querySelectorAll('a').length).toBe(0);
    for (const node of screen.getAllByRole('button')) expect(node.tagName).toBe('BUTTON');
  });

  it('uses Area as a disclosure and routes only Track rows through onGo', async () => {
    const onGo = vi.fn();
    renderSidebar({ tracks: [track({ id: 'w9', title: 'Row' })], onGo });
    await userEvent.click(screen.getByRole('button', { name: 'Collapse area Work' }));
    expect(onGo).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: 'Expand area Work' }));
    await userEvent.click(screen.getByRole('button', { name: /^Track Row/ }));
    const targets: unknown[] = onGo.mock.calls.map((call) => (call as unknown[])[0]);
    expect(targets).toEqual([{ name: 'track', trackId: 'w9' }]);
  });
});

describe('active row', () => {
  it('marks only the open Track with aria-current', () => {
    renderSidebar({ tracks: [track({ id: 'w9', title: 'Row' })], currentPath: '/track/w9' });
    expect(screen.getByRole('button', { name: /^Track Row/ }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('button', { name: 'Collapse area Work' }).getAttribute('aria-current')).toBeNull();
  });

  /* "Waiting on you" and "Pinned" are shortcuts into the tree; a location is shown
   * where the thing lives. */
  it('marks the open track once, in its area, not in the shortcut sections', () => {
    const open = track({ id: 'w9', title: 'Row', lifecycle: 'blocked', attention: 'input', pinnedAt: 10 });
    renderSidebar({ tracks: [open], currentPath: '/track/w9' });
    const rows = screen.getAllByRole('button', { name: /^Track Row/ });
    expect(rows).toHaveLength(3);
    expect(rows.filter((row) => row.getAttribute('aria-current') === 'page')).toHaveLength(1);
  });
});
