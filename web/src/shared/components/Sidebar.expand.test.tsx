import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { SessionContext } from '../../app/SessionProvider';
import type { Cove, Route, Wave } from '../../types';
import { Sidebar } from './Sidebar';

const EXPANDED_COVES_STORAGE_KEY = 'calm:sidebar:expandedCoves';

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

function makeCove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Atlas', subtitle: '', color: '#5a9', ...overrides };
}

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1',
    coveId: 'c1',
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
  coves = [makeCove()],
  waves,
  route = { name: 'today' },
  onGo = () => {},
  onPinWave,
}: {
  coves?: Cove[];
  waves: Wave[];
  route?: Route;
  onGo?: (r: Route) => void;
  onPinWave?: (waveId: string, pin: boolean) => void | Promise<void>;
}) {
  return render(
    wrap(
      <Sidebar
        coves={coves}
        waves={waves}
        route={route}
        onGo={onGo}
        onPinWave={onPinWave}
      />,
    ),
  );
}

describe('Sidebar cove expansion', () => {
  it('defaults coves to collapsed', () => {
    renderSidebar({
      waves: [
        makeWave({ id: 'w1', title: 'Harbor cleanup' }),
        makeWave({ id: 'w2', title: 'Tide report' }),
      ],
    });

    const chevron = screen.getByRole('button', { name: 'Expand cove Atlas' });
    expect(chevron).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('Harbor cleanup')).toBeNull();
    expect(screen.queryByText('Tide report')).toBeNull();
  });

  it('expands a cove from the chevron', () => {
    renderSidebar({
      waves: [
        makeWave({ id: 'w1', title: 'Harbor cleanup' }),
        makeWave({ id: 'w2', title: 'Tide report' }),
      ],
    });

    fireEvent.click(screen.getByRole('button', { name: 'Expand cove Atlas' }));

    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
    expect(screen.getByText('Tide report')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Collapse cove Atlas' }))
      .toHaveAttribute('aria-expanded', 'true');
  });

  it('collapses a cove from the chevron', () => {
    renderSidebar({ waves: [makeWave({ title: 'Harbor cleanup' })] });

    fireEvent.click(screen.getByRole('button', { name: 'Expand cove Atlas' }));
    fireEvent.click(screen.getByRole('button', { name: 'Collapse cove Atlas' }));

    expect(screen.getByRole('button', { name: 'Expand cove Atlas' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('Harbor cleanup')).toBeNull();
  });

  it('keeps cove row navigation on the nav button without changing expansion', () => {
    const onGo = vi.fn();
    renderSidebar({ waves: [makeWave()], onGo });

    const covesNav = screen.getByRole('navigation', { name: 'Coves' });
    fireEvent.click(within(covesNav).getByRole('button', { name: 'Atlas' }));

    expect(onGo).toHaveBeenCalledWith({ name: 'cove', coveId: 'c1' });
    expect(screen.getByRole('button', { name: 'Expand cove Atlas' }))
      .toHaveAttribute('aria-expanded', 'false');
  });

  it('does not navigate when the chevron is clicked', () => {
    const onGo = vi.fn();
    renderSidebar({ waves: [makeWave()], onGo });

    fireEvent.click(screen.getByRole('button', { name: 'Expand cove Atlas' }));

    expect(onGo).not.toHaveBeenCalled();
  });

  it('filters pinned waves from the expanded inline list', () => {
    renderSidebar({
      waves: [
        makeWave({ id: 'w-pin', title: 'Pinned wave', pinnedAt: 1000 }),
        makeWave({ id: 'w-open', title: 'Open wave' }),
      ],
      onPinWave: vi.fn(),
    });

    expect(within(screen.getByRole('region', { name: 'Pinned' }))
      .getByText('Pinned wave')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Expand cove Atlas' }));
    const inline = screen.getByRole('group', { name: 'Waves in Atlas' });

    expect(within(inline).getByText('Open wave')).toBeTruthy();
    expect(within(inline).queryByText('Pinned wave')).toBeNull();
  });

  it('persists expanded coves in localStorage across remounts', () => {
    const props = { waves: [makeWave({ title: 'Harbor cleanup' })] };
    const { unmount } = renderSidebar(props);

    fireEvent.click(screen.getByRole('button', { name: 'Expand cove Atlas' }));
    unmount();
    renderSidebar(props);

    expect(screen.getByRole('button', { name: 'Collapse cove Atlas' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
  });

  it('still toggles when localStorage writes throw', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('localStorage blocked');
    });
    renderSidebar({ waves: [makeWave({ title: 'Harbor cleanup' })] });

    fireEvent.click(screen.getByRole('button', { name: 'Expand cove Atlas' }));

    expect(screen.getByRole('button', { name: 'Collapse cove Atlas' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('Harbor cleanup')).toBeTruthy();
  });

  it('highlights the active wave inside an expanded cove', () => {
    window.localStorage.setItem(
      EXPANDED_COVES_STORAGE_KEY,
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
});
