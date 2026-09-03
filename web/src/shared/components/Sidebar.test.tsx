import { describe, it, expect, vi, afterEach } from 'vitest';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { SessionContext } from '../../app/SessionProvider';
import type { Area, Route, Track } from '../../types';
import { Sidebar } from './Sidebar';

afterEach(() => {
  cleanup();
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

function makeArea(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Atlas', subtitle: '', color: '#5a9', ...overrides };
}

function makeTrack(overrides: Partial<Track> = {}): Track {
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
  tracks = [makeTrack()],
  route = { name: 'today' },
  onGo = () => {},
  onPinTrack,
  onDeleteTrack,
}: {
  areas?: Area[];
  tracks?: Track[];
  route?: Route;
  onGo?: (r: Route) => void;
  onPinTrack?: (trackId: string, pin: boolean) => void | Promise<void>;
  onDeleteTrack?: (trackId: string) => void | Promise<void>;
} = {}) {
  return render(
    wrap(
      <Sidebar
        areas={areas}
        tracks={tracks}
        route={route}
        onGo={onGo}
        onPinTrack={onPinTrack}
        onDeleteTrack={onDeleteTrack}
      />,
    ),
  );
}

async function expandAtlas(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole('button', { name: 'Expand area Atlas' }));
  return screen.getByRole('group', { name: 'Tracks in Atlas' });
}

describe('Sidebar track delete', () => {
  it('Per-row × on a sidebar TrackRow shows a confirm dialog and calls onDeleteTrack on confirm', async () => {
    const user = userEvent.setup();
    const onDeleteTrack = vi.fn();
    renderSidebar({ onDeleteTrack });

    const inline = await expandAtlas(user);
    await user.click(
      within(inline).getByRole('button', { name: 'Delete track "Harbor cleanup"' }),
    );

    const dialog = screen.getByRole('dialog', { name: 'Delete track?' });
    expect(dialog).toHaveTextContent('Delete track "Harbor cleanup"?');
    await user.click(within(dialog).getByRole('button', { name: 'Delete track' }));

    expect(screen.queryByRole('dialog', { name: 'Delete track?' })).toBeNull();
    expect(onDeleteTrack).toHaveBeenCalledTimes(1);
    expect(onDeleteTrack).toHaveBeenCalledWith('w1');
  });

  it('Cancel closes the dialog without invoking onDeleteTrack', async () => {
    const user = userEvent.setup();
    const onDeleteTrack = vi.fn();
    renderSidebar({ onDeleteTrack });

    const inline = await expandAtlas(user);
    await user.click(
      within(inline).getByRole('button', { name: 'Delete track "Harbor cleanup"' }),
    );
    const dialog = screen.getByRole('dialog', { name: 'Delete track?' });
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));

    expect(screen.queryByRole('dialog', { name: 'Delete track?' })).toBeNull();
    expect(onDeleteTrack).not.toHaveBeenCalled();
  });

  it('Pin button is on the left of the row (DOM order: pin → title → delete)', () => {
    const track = makeTrack({ pinnedAt: 1000 });
    renderSidebar({
      tracks: [track],
      onPinTrack: vi.fn(),
      onDeleteTrack: vi.fn(),
    });

    const pinned = screen.getByRole('region', { name: 'Pinned' });
    const row = within(pinned)
      .getByText('Harbor cleanup')
      .closest('.side-track-row');
    expect(row).not.toBeNull();

    const buttons = within(row as HTMLElement).getAllByRole('button');
    expect(buttons).toHaveLength(3);
    expect(buttons[0]).toHaveAccessibleName('Unpin track');
    expect(buttons[1]).toHaveTextContent('Harbor cleanup');
    expect(buttons[2]).toHaveAccessibleName('Delete track "Harbor cleanup"');
  });
});
