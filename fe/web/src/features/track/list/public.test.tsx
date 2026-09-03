// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../../core/domain/track.ts';
import { TrackList } from './public.tsx';

afterEach(cleanup);

function area(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Alpha', sort: 1, lifecycle: 'done', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

function titlesInOrder(): string[] {
  return screen.getAllByRole('button', { name: /^Track / }).map((node) => node.getAttribute('aria-label') ?? '');
}

describe('TrackList', () => {
  it('renders the empty message when there are no tracks', () => {
    render(<TrackList tracks={[]} areas={[]} onOpenTrack={vi.fn()} emptyMessage="No tracks in this area yet." />);
    expect(screen.getByText('No tracks in this area yet.')).toBeTruthy();
  });

  it('does not list archived tracks', () => {
    render(<TrackList
      tracks={[track({ title: 'Filed away', archivedAt: 42 })]}
      areas={[area()]}
      onOpenTrack={vi.fn()}
      emptyMessage="No visible tracks."
    />);
    expect(screen.queryByRole('button', { name: /Filed away/ })).toBeNull();
    expect(screen.getByText('No visible tracks.')).toBeTruthy();
  });

  it('orders waiting before running before quiet', () => {
    render(<TrackList
      tracks={[
        track({ id: 'q', title: 'Quiet', lifecycle: 'done' }),
        track({ id: 'r', title: 'Running', lifecycle: 'working' }),
        track({ id: 'a', title: 'Attention', lifecycle: 'blocked' }),
      ]}
      areas={[area()]}
      onOpenTrack={vi.fn()}
      emptyMessage="empty"
    />);
    expect(titlesInOrder().map((label) => label.split(',')[0])).toEqual([
      'Track Attention', 'Track Running', 'Track Quiet',
    ]);
  });

  it('promotes a track whose card needs input even when its lifecycle is quiet', () => {
    render(<TrackList
      tracks={[
        track({ id: 'r', title: 'Running', lifecycle: 'working' }),
        track({ id: 'n', title: 'Needy', lifecycle: 'draft', anyCardNeedsInput: true }),
      ]}
      areas={[area()]}
      onOpenTrack={vi.fn()}
      emptyMessage="empty"
    />);
    expect(titlesInOrder()[0]?.startsWith('Track Needy')).toBe(true);
  });

  it('opens the track the row stands for', async () => {
    const onOpenTrack = vi.fn();
    render(<TrackList tracks={[track()]} areas={[area()]} onOpenTrack={onOpenTrack} emptyMessage="empty" />);
    await userEvent.click(screen.getByRole('button', { name: /^Track Alpha/ }));
    expect(onOpenTrack).toHaveBeenCalledWith('w1');
  });

  it('names the area only when showArea is set', () => {
    const { unmount } = render(
      <TrackList tracks={[track()]} areas={[area()]} onOpenTrack={vi.fn()} emptyMessage="empty" />,
    );
    expect(screen.getByRole('button', { name: 'Track Alpha, Done' })).toBeTruthy();
    unmount();

    render(<TrackList tracks={[track()]} areas={[area()]} showArea onOpenTrack={vi.fn()} emptyMessage="empty" />);
    expect(screen.getByRole('button', { name: 'Track Alpha, Done, in area Work' })).toBeTruthy();
  });

  it('falls back to Unknown area when the track points at an area we cannot see', () => {
    render(<TrackList
      tracks={[track({ areaId: 'gone' })]}
      areas={[area()]}
      showArea
      onOpenTrack={vi.fn()}
      emptyMessage="empty"
    />);
    expect(screen.getByRole('button', { name: /in area Unknown area$/ })).toBeTruthy();
  });

  it('marks the active track with aria-current', () => {
    render(<TrackList
      tracks={[track({ id: 'w1', title: 'Alpha' }), track({ id: 'w2', title: 'Beta' })]}
      areas={[area()]}
      activeTrackId="w2"
      onOpenTrack={vi.fn()}
      emptyMessage="empty"
    />);
    expect(screen.getByRole('button', { name: /^Track Beta/ }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('button', { name: /^Track Alpha/ }).getAttribute('aria-current')).toBeNull();
  });

  it('hides pin and delete unless the callbacks are supplied', () => {
    render(<TrackList tracks={[track()]} areas={[area()]} onOpenTrack={vi.fn()} emptyMessage="empty" />);
    expect(screen.queryByRole('button', { name: 'Pin Alpha' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Delete Alpha' })).toBeNull();
  });

  it('forwards pin and delete to the callbacks', async () => {
    const onSetPinned = vi.fn();
    const onDeleteTrack = vi.fn();
    render(<TrackList
      tracks={[track()]}
      areas={[area()]}
      onOpenTrack={vi.fn()}
      onSetPinned={onSetPinned}
      onDeleteTrack={onDeleteTrack}
      emptyMessage="empty"
    />);
    await userEvent.click(screen.getByRole('button', { name: 'Pin Alpha' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete Alpha' }));
    expect(onSetPinned).toHaveBeenCalledWith('w1', true);
    expect(onDeleteTrack).toHaveBeenCalledWith('w1');
  });
});
