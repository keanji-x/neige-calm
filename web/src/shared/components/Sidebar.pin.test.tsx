// Component-level tests for the Sidebar pin-track feature.
//
// Pinned tracks appear in a dedicated "Pinned" section below "Waiting on you".
// A pin/unpin button is revealed on row hover. Pinned tracks that need
// attention also appear in "Waiting on you" and increment area warn badges.

import { describe, it, expect, vi, afterEach } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { SessionContext } from '../../app/SessionProvider';
import { Sidebar } from './Sidebar';
import type { Area, Track } from '../../types';

afterEach(cleanup);

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

function makeArea(id = 'c1'): Area {
  return { id, name: 'Atlas', subtitle: '', color: '#5a9' };
}

function makeTrack(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1',
    areaId: 'c1',
    title: 'My track',
    lifecycle: 'blocked',
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

function sidebarProps(
  tracks: Track[],
  onPinTrack?: (id: string, pin: boolean) => void,
) {
  return {
    areas: [makeArea()],
    tracks,
    route: { name: 'today' } as const,
    onGo: () => {},
    onPinTrack,
  };
}

describe('Sidebar pinned section', () => {
  it('renders no Pinned section when all tracks are unpinned', () => {
    const track = makeTrack({ lifecycle: 'draft', anyCardNeedsInput: false });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    expect(screen.queryByRole('region', { name: 'Pinned' })).toBeNull();
  });

  it('renders a Pinned section when a track has pinnedAt set', () => {
    const track = makeTrack({ lifecycle: 'draft', anyCardNeedsInput: false, pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    expect(screen.getByRole('region', { name: 'Pinned' })).toBeTruthy();
    expect(screen.getByText('My track')).toBeTruthy();
  });

  it('renders the fallback label for a pinned track with an empty title', () => {
    const track = makeTrack({
      title: '',
      lifecycle: 'draft',
      anyCardNeedsInput: false,
      pinnedAt: 1000,
    });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    expect(screen.getByRole('region', { name: 'Pinned' })).toHaveTextContent('Untitled track');
  });

  it('pinned track appears in both Pinned and Waiting on you', () => {
    const track = makeTrack({ lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    const waiting = screen.getByRole('region', { name: 'Waiting on you' });
    expect(pinned).toBeTruthy();
    expect(waiting).toBeTruthy();
    expect(pinned).toHaveTextContent('My track');
    expect(waiting).toHaveTextContent('My track');
  });

  it('renders Waiting on you before Pinned when a track is both pinned and waiting', () => {
    const track = makeTrack({ lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    const waiting = screen.getByRole('region', { name: 'Waiting on you' });
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    expect(
      waiting.compareDocumentPosition(pinned) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it('unpinned track that needs attention appears only in Waiting on you', () => {
    const track = makeTrack({ lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    expect(screen.queryByRole('region', { name: 'Pinned' })).toBeNull();
    expect(screen.getByRole('region', { name: 'Waiting on you' })).toBeTruthy();
  });

  it('waiting track renders Waiting on you as an attention zone', () => {
    const track = makeTrack({ lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    const waiting = screen.getByRole('region', { name: 'Waiting on you' });
    expect(waiting.classList.contains('attn-zone')).toBe(true);
  });

  it('calls onPinTrack(id, false) when pin button is clicked on a pinned track', () => {
    const onPinTrack = vi.fn();
    const track = makeTrack({
      id: 'w-pin',
      lifecycle: 'draft',
      anyCardNeedsInput: false,
      pinnedAt: 1000,
    });
    render(wrap(<Sidebar {...sidebarProps([track], onPinTrack)} />));
    const btn = screen.getByRole('button', { name: 'Unpin track' });
    fireEvent.click(btn);
    expect(onPinTrack).toHaveBeenCalledWith('w-pin', false);
  });

  it('calls onPinTrack(id, true) when pin button is clicked on an unpinned track', () => {
    const onPinTrack = vi.fn();
    // waiting track = blocked lifecycle
    const track = makeTrack({ id: 'w-unpin', lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([track], onPinTrack)} />));
    const btn = screen.getByRole('button', { name: 'Pin track' });
    fireEvent.click(btn);
    expect(onPinTrack).toHaveBeenCalledWith('w-unpin', true);
  });

  it('sorts pinned tracks by pinnedAt ascending', () => {
    const w1 = makeTrack({ id: 'w1', title: 'First', pinnedAt: 1000 });
    const w2 = makeTrack({ id: 'w2', title: 'Second', pinnedAt: 500 });
    render(wrap(<Sidebar {...sidebarProps([w1, w2])} />));
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    const buttons = Array.from(pinned.querySelectorAll<HTMLButtonElement>('button.side-track'));
    // "Second" (pinnedAt=500) must come before "First" (pinnedAt=1000)
    expect(buttons[0]).toHaveTextContent('Second');
    expect(buttons[1]).toHaveTextContent('First');
  });
});

describe('Sidebar per-area badge parity with Waiting section', () => {
  it('pinned blocked track increments the area warn waiting badge', () => {
    const track = makeTrack({ id: 'w-pinned-blocked', lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    expect(screen.getByRole('region', { name: 'Pinned' })).toBeTruthy();
    expect(screen.getByRole('region', { name: 'Waiting on you' })).toBeTruthy();
    const badge = document.querySelector('.area-nav-badge.warn');
    expect(badge).toBeTruthy();
    expect(badge?.textContent).toBe('1');
  });

  it('unpinned blocked track increments the area red waiting badge', () => {
    const track = makeTrack({ id: 'w-unblocked', lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    const badge = document.querySelector('.area-nav-badge');
    expect(badge).toBeTruthy();
    expect(badge?.classList.contains('warn')).toBe(true);
    expect(badge?.textContent).toBe('1');
  });

  it('pinned attention row carries the attention class for warn title styling', () => {
    const track = makeTrack({ id: 'w-pinned-attention', lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    const row = pinned.querySelector('.side-track-row.attention');
    expect(row).toBeTruthy();
    expect(row?.querySelector('.side-track-title')).toHaveTextContent('My track');
  });

  it('waiting attention row carries the attention class for warn title styling', () => {
    const track = makeTrack({ id: 'w-attention', lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([track])} />));
    const waiting = screen.getByRole('region', { name: 'Waiting on you' });
    const row = waiting.querySelector('.side-track-row.attention');
    expect(row).toBeTruthy();
    expect(row?.querySelector('.side-track-title')).toHaveTextContent('My track');
  });

  it('inline area row carries the attention class for warn title styling', () => {
    const onPinTrack = vi.fn();
    const track = makeTrack({ id: 'w-inline-attention', lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([track], onPinTrack)} />));

    fireEvent.click(screen.getByRole('button', { name: /Expand area Atlas/ }));

    const inline = screen.getByRole('group', { name: 'Tracks in Atlas' });
    expect(within(inline).getByText('My track')).toBeTruthy();
    expect(inline.querySelector('.side-track-row.attention .side-track-title')).toBeTruthy();
  });
});

describe('Sidebar TrackRow area-name span', () => {
  it('track with no matching area renders without the area text span', () => {
    // areaId does not match any area in the list → orphan track
    const track = makeTrack({ id: 'w-orphan', areaId: 'nonexistent', lifecycle: 'blocked', pinnedAt: null });
    render(
      wrap(
        <Sidebar
          areas={[makeArea('c1')]}
          tracks={[track]}
          route={{ name: 'today' }}
          onGo={() => {}}
        />,
      ),
    );
    // No .side-track-area span rendered when area is not found.
    expect(document.querySelector('.side-track-area')).toBeNull();
    // The track nav button is still present.
    expect(screen.getByRole('button', { name: /My track/i })).toBeTruthy();
  });
});
