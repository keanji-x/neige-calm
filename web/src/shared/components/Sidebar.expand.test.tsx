import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { SessionContext } from '../../app/SessionProvider';
import type { Area, Route, Wave } from '../../types';
import { Sidebar } from './Sidebar';

const EXPANDED_AREAS_STORAGE_KEY = 'calm:sidebar:expandedAreas';
const SIDEBAR_COLLAPSED_STORAGE_KEY = 'calm:sidebar:collapsed';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  window.localStorage.clear();
});

const STUB_SESSION = {
  userId: 'u-test',
  displayName: 'Test User',
  role: 'owner',
  sessionId: 's-test',
};

function wrap(children: ReactNode) {
  return (
    <SessionContext.Provider value={STUB_SESSION}>
      {children}
    </SessionContext.Provider>
  );
}

function sidebarNode({
  areas = [makeArea()],
  waves,
  route = { name: 'today' },
  onGo = () => {},
  onPinWave,
}: {
  areas?: Area[];
  waves: Wave[];
  route?: Route;
  onGo?: (r: Route) => void;
  onPinWave?: (waveId: string, pin: boolean) => void | Promise<void>;
}) {
  return wrap(
    <Sidebar
      areas={areas}
      waves={waves}
      route={route}
      onGo={onGo}
      onPinWave={onPinWave}
    />,
  );
}

function makeArea(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Atlas', subtitle: '', color: '#5a9', ...overrides };
}

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1',
    areaId: 'c1',
    title: 'Harbor cleanup',
    lifecycle: 'draft',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    createdAt: 0,
    terminalAt: null,
    pinnedAt: null,
    ...overrides,
  };
}

function renderSidebar({
  areas = [makeArea()],
  waves,
  route = { name: 'today' },
  onGo = () => {},
  onPinWave,
}: {
  areas?: Area[];
  waves: Wave[];
  route?: Route;
  onGo?: (r: Route) => void;
  onPinWave?: (waveId: string, pin: boolean) => void | Promise<void>;
}) {
  return render(sidebarNode({ areas, waves, route, onGo, onPinWave }));
}

describe('Sidebar area expansion', () => {
  it('collapses to a rail and expands from the keyboard-reachable toggle', async () => {
    const user = userEvent.setup();
    renderSidebar({ waves: [makeWave()] });

    await user.tab();
    expect(screen.getByRole('button', { name: 'Today' })).toHaveFocus();

    await user.tab();
    expect(screen.getByRole('button', { name: 'Collapse sidebar' })).toHaveFocus();

    await user.keyboard('{Enter}');

    expect(screen.getByRole('button', { name: 'Expand sidebar' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(
      screen.queryByRole('navigation', { name: 'Sidebar navigation' }),
    ).toBeNull();

    await user.keyboard('{Enter}');

    expect(screen.getByRole('button', { name: 'Collapse sidebar' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(
      screen.getByRole('navigation', { name: 'Sidebar navigation' }),
    ).toBeTruthy();
  });

  it('persists the collapsed rail across remounts', () => {
    const props = { waves: [makeWave()] };
    const { unmount } = renderSidebar(props);

    fireEvent.click(screen.getByRole('button', { name: 'Collapse sidebar' }));

    expect(window.localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY)).toBe('true');
    unmount();
    renderSidebar(props);

    expect(screen.getByRole('button', { name: 'Expand sidebar' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(
      screen.queryByRole('navigation', { name: 'Sidebar navigation' }),
    ).toBeNull();
  });

  it('defaults areas to collapsed', () => {
    renderSidebar({
      waves: [
        makeWave({ id: 'w1', title: 'Harbor cleanup' }),
        makeWave({ id: 'w2', title: 'Tide report' }),
      ],
    });

    const chevron = screen.getByRole('button', { name: 'Expand area Atlas' });
    expect(chevron).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('Harbor cleanup')).toBeNull();
    expect(screen.queryByText('Tide report')).toBeNull();
  });

  it('expands an area from the chevron', () => {
    renderSidebar({
      waves: [
        makeWave({ id: 'w1', title: 'Harbor cleanup' }),
        makeWave({ id: 'w2', title: 'Tide report' }),
      ],
    });

    fireEvent.click(screen.getByRole('button', { name: 'Expand area Atlas' }));

    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
    expect(screen.getByText('Tide report')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Collapse area Atlas' }))
      .toHaveAttribute('aria-expanded', 'true');
  });

  it('collapses an area from the chevron', () => {
    renderSidebar({ waves: [makeWave({ title: 'Harbor cleanup' })] });

    fireEvent.click(screen.getByRole('button', { name: 'Expand area Atlas' }));
    fireEvent.click(screen.getByRole('button', { name: 'Collapse area Atlas' }));

    expect(screen.getByRole('button', { name: 'Expand area Atlas' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('Harbor cleanup')).toBeNull();
  });

  it('keeps area row navigation on the nav button without changing expansion', () => {
    const onGo = vi.fn();
    renderSidebar({ waves: [makeWave()], onGo });

    const areasNav = screen.getByRole('navigation', { name: 'Areas' });
    fireEvent.click(within(areasNav).getByRole('button', { name: 'Atlas' }));

    expect(onGo).toHaveBeenCalledWith({ name: 'area', areaId: 'c1' });
    expect(screen.getByRole('button', { name: 'Expand area Atlas' }))
      .toHaveAttribute('aria-expanded', 'false');
  });

  it('does not navigate when the chevron is clicked', () => {
    const onGo = vi.fn();
    renderSidebar({ waves: [makeWave()], onGo });

    fireEvent.click(screen.getByRole('button', { name: 'Expand area Atlas' }));

    expect(onGo).not.toHaveBeenCalled();
  });

  it('shows pinned waves in both the pinned section and expanded inline list', () => {
    renderSidebar({
      waves: [
        makeWave({ id: 'w-pin', title: 'Pinned wave', pinnedAt: 1000 }),
        makeWave({ id: 'w-open', title: 'Open wave' }),
      ],
      onPinWave: vi.fn(),
    });

    expect(within(screen.getByRole('region', { name: 'Pinned' }))
      .getByText('Pinned wave')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Expand area Atlas' }));
    const inline = screen.getByRole('group', { name: 'Waves in Atlas' });

    expect(within(inline).getByText('Open wave')).toBeTruthy();
    // Pinning is a shortcut, not relocation: the wave remains in its area.
    expect(within(inline).getByText('Pinned wave')).toBeTruthy();
  });

  it('persists expanded areas in localStorage across remounts', () => {
    const props = { waves: [makeWave({ title: 'Harbor cleanup' })] };
    const { unmount } = renderSidebar(props);

    fireEvent.click(screen.getByRole('button', { name: 'Expand area Atlas' }));
    unmount();
    renderSidebar(props);

    expect(screen.getByRole('button', { name: 'Collapse area Atlas' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
  });

  it('still toggles when localStorage writes throw', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('localStorage blocked');
    });
    renderSidebar({ waves: [makeWave({ title: 'Harbor cleanup' })] });

    fireEvent.click(screen.getByRole('button', { name: 'Expand area Atlas' }));

    expect(screen.getByRole('button', { name: 'Collapse area Atlas' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
  });

  it('highlights the active wave inside an expanded area', () => {
    window.localStorage.setItem(
      EXPANDED_AREAS_STORAGE_KEY,
      JSON.stringify({ c1: true }),
    );
    const wave = makeWave({ id: 'w-active', title: 'Active wave' });
    renderSidebar({
      waves: [wave],
      route: { name: 'wave', id: 'w-active' },
    });

    const inline = screen.getByRole('group', { name: 'Waves in Atlas' });
    const row = within(inline)
      .getByRole('button', { name: 'Active wave' })
      .closest('.side-wave-row');

    expect(row).toHaveClass('active');
  });

  it('auto-expands the active wave area on wave route navigation', async () => {
    const waves = [
      makeWave({ id: 'w1', title: 'Harbor cleanup' }),
      makeWave({ id: 'w2', title: 'Tide report' }),
    ];
    const { rerender } = renderSidebar({ waves, route: { name: 'today' } });

    rerender(sidebarNode({ waves, route: { name: 'wave', id: 'w1' } }));

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Collapse area Atlas' }))
        .toHaveAttribute('aria-expanded', 'true');
    });
    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
    expect(screen.getByText('Tide report')).toBeTruthy();
  });

  it('keeps a manually collapsed active area collapsed on same-wave rerender', async () => {
    const wave = makeWave({ id: 'w1', title: 'Harbor cleanup' });
    const { rerender } = renderSidebar({
      waves: [wave],
      route: { name: 'wave', id: 'w1' },
    });

    await screen.findByText('Harbor cleanup');
    fireEvent.click(screen.getByRole('button', { name: 'Collapse area Atlas' }));
    rerender(sidebarNode({
      waves: [wave],
      route: { name: 'wave', id: 'w1' },
    }));

    expect(screen.getByRole('button', { name: 'Expand area Atlas' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('Harbor cleanup')).toBeNull();
  });

  it('re-expands a manually collapsed area when navigating to another wave in it', async () => {
    const waves = [
      makeWave({ id: 'w1', title: 'Harbor cleanup' }),
      makeWave({ id: 'w2', title: 'Tide report' }),
    ];
    const { rerender } = renderSidebar({
      waves,
      route: { name: 'wave', id: 'w1' },
    });

    await screen.findByText('Harbor cleanup');
    fireEvent.click(screen.getByRole('button', { name: 'Collapse area Atlas' }));
    rerender(sidebarNode({ waves, route: { name: 'wave', id: 'w2' } }));

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Collapse area Atlas' }))
        .toHaveAttribute('aria-expanded', 'true');
    });
    expect(screen.getByText('Tide report')).toBeTruthy();
  });

  it('expands the new active area without changing the previous area state', async () => {
    const areas = [
      makeArea({ id: 'c1', name: 'Atlas' }),
      makeArea({ id: 'c2', name: 'Boreal' }),
    ];
    const waves = [
      makeWave({ id: 'w1', areaId: 'c1', title: 'Harbor cleanup' }),
      makeWave({ id: 'w2', areaId: 'c2', title: 'Ice survey' }),
    ];
    const { rerender } = renderSidebar({
      areas,
      waves,
      route: { name: 'wave', id: 'w1' },
    });

    await screen.findByText('Harbor cleanup');
    fireEvent.click(screen.getByRole('button', { name: 'Collapse area Atlas' }));
    rerender(sidebarNode({
      areas,
      waves,
      route: { name: 'wave', id: 'w2' },
    }));

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Collapse area Boreal' }))
        .toHaveAttribute('aria-expanded', 'true');
    });
    expect(screen.getByRole('button', { name: 'Expand area Atlas' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByText('Ice survey')).toBeTruthy();
    expect(screen.queryByText('Harbor cleanup')).toBeNull();
  });

  it('does not touch expansion state when leaving a wave route', async () => {
    const wave = makeWave({ id: 'w1', title: 'Harbor cleanup' });
    const { rerender } = renderSidebar({
      waves: [wave],
      route: { name: 'wave', id: 'w1' },
    });

    await screen.findByText('Harbor cleanup');
    fireEvent.click(screen.getByRole('button', { name: 'Collapse area Atlas' }));
    rerender(sidebarNode({ waves: [wave], route: { name: 'today' } }));

    expect(screen.getByRole('button', { name: 'Expand area Atlas' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('Harbor cleanup')).toBeNull();
  });

  it('persists auto-expanded areas in localStorage across remounts', async () => {
    const props = {
      waves: [makeWave({ id: 'w1', title: 'Harbor cleanup' })],
      route: { name: 'wave', id: 'w1' } as Route,
    };
    const { unmount } = renderSidebar(props);

    await screen.findByText('Harbor cleanup');
    unmount();
    renderSidebar({ waves: props.waves });

    expect(screen.getByRole('button', { name: 'Collapse area Atlas' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
  });

  it('auto-expands when the active wave arrives after the route', async () => {
    const wave = makeWave({ id: 'w1', title: 'Harbor cleanup' });
    const { rerender } = renderSidebar({
      waves: [],
      route: { name: 'wave', id: 'w1' },
    });

    expect(screen.getByRole('button', { name: 'Expand area Atlas' }))
      .toHaveAttribute('aria-expanded', 'false');

    rerender(sidebarNode({
      waves: [wave],
      route: { name: 'wave', id: 'w1' },
    }));

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Collapse area Atlas' }))
        .toHaveAttribute('aria-expanded', 'true');
    });
    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
  });

  it('scrolls the active wave row into view', async () => {
    window.localStorage.setItem(
      EXPANDED_AREAS_STORAGE_KEY,
      JSON.stringify({ c1: true }),
    );
    const scrolledElements: Element[] = [];
    const scrollIntoView = vi.fn(function scrollMock(this: Element) {
      scrolledElements.push(this);
    });
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      writable: true,
      value: scrollIntoView,
    });
    const wave = makeWave({ id: 'w1', title: 'Harbor cleanup' });
    renderSidebar({
      waves: [wave],
      route: { name: 'wave', id: 'w1' },
    });

    await waitFor(() => {
      expect(scrollIntoView).toHaveBeenCalledWith({
        block: 'nearest',
        behavior: 'smooth',
      });
    });
    const inline = screen.getByRole('group', { name: 'Waves in Atlas' });
    const row = within(inline)
      .getByRole('button', { name: 'Harbor cleanup' })
      .closest('.side-wave-row');
    expect(row).toBeTruthy();
    expect(scrolledElements).toContain(row);
  });
});
