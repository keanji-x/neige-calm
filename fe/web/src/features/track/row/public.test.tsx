// @vitest-environment jsdom
// INV-DUP-009's one row, tested directly.
//
// Three surfaces render it and none of them can test it: the rail composes it
// through `app/shell`, the area page and Today receive it by injection because
// `features/**` may not import a sibling domain. Their suites therefore use
// stand-ins, and a stand-in cannot prove the row is a button, carries a
// composed accessible name, or keeps its pin reachable. This file is where
// those live — against the real component.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { NEUTRAL_ACTIVITY, type Track } from '../../../../../core/domain/track.ts';
import { TrackRow, relativeTime } from './public.tsx';

afterEach(cleanup);

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Open track', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('INV-A11Y-061 navigation shape', () => {
  it('is a button and emits no native link', () => {
    const { container } = render(<TrackRow track={track()} onOpen={vi.fn()} nowMs={NOW} />);
    expect(container.querySelectorAll('a').length).toBe(0);
    expect(screen.getByRole('button', { name: /^Track Open track/ }).tagName).toBe('BUTTON');
  });

  it('opens through the callback, never a href', async () => {
    const onOpen = vi.fn();
    render(<TrackRow track={track()} onOpen={onOpen} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: /^Track Open track/ }));
    expect(onOpen).toHaveBeenCalledWith('w1');
  });
});

describe('accessible name', () => {
  // The status dot is `aria-hidden` decoration — it is the *name* that has to
  // carry lifecycle and attention, on every variant, including the rail where
  // the dot is the only thing a sighted user sees.
  it('names the lifecycle, and the attention state when there is one', () => {
    render(<TrackRow track={track({ lifecycle: 'blocked' })} areaName="Work" onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', {
      name: 'Track Open track, waiting on you, Blocked, in area Work',
    })).toBeTruthy();
  });

  it('names the area only when the surface supplies one', () => {
    render(<TrackRow track={track()} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Track Open track, running, Working' })).toBeTruthy();
  });

  it('uses the untitled label rather than an empty name', () => {
    render(<TrackRow track={track({ title: '   ' })} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: /^Track Untitled track/ })).toBeTruthy();
  });
});

describe('INV-SIDEBAR-012 the pin is always reachable, and names its action', () => {
  it('carries aria-pressed in both states', async () => {
    const onSetPinned = vi.fn();
    const view = render(<TrackRow track={track()} onOpen={vi.fn()} onSetPinned={onSetPinned} nowMs={NOW} />);
    const pin = screen.getByRole('button', { name: 'Pin Open track' });
    expect(pin.getAttribute('aria-pressed')).toBe('false');
    await userEvent.click(pin);
    expect(onSetPinned).toHaveBeenCalledWith('w1', true);

    view.rerender(<TrackRow track={track({ pinnedAt: 10 })} onOpen={vi.fn()} onSetPinned={onSetPinned} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Unpin Open track' }).getAttribute('aria-pressed')).toBe('true');
  });

  it('keeps the arrow-up icon while the accessible action changes', () => {
    const view = render(<TrackRow track={track()} onOpen={vi.fn()} onSetPinned={vi.fn()} nowMs={NOW} />);
    const before = screen.getByRole('button', { name: 'Pin Open track' }).querySelector('svg');
    expect(before).toBeTruthy();
    view.rerender(<TrackRow track={track({ pinnedAt: 10 })} onOpen={vi.fn()} onSetPinned={vi.fn()} nowMs={NOW} />);
    const after = screen.getByRole('button', { name: 'Unpin Open track' }).querySelector('svg');
    const expectedPaths = ['M8 12.5V3.5', 'M4 7.5 8 3.5l4 4'];
    expect([...before!.querySelectorAll('path')].map((path) => path.getAttribute('d'))).toEqual(expectedPaths);
    expect([...after!.querySelectorAll('path')].map((path) => path.getAttribute('d'))).toEqual(expectedPaths);
  });

  it('renders no pin and no delete unless the surface supplies the callback', () => {
    render(<TrackRow track={track()} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.queryByRole('button', { name: /^(Pin|Unpin|Delete) / })).toBeNull();
  });
});

describe('§6.3 variants differ in what they render, not in what they are', () => {
  it('drops the lifecycle line and the relative time in the rail', () => {
    const { container } = render(<TrackRow track={track()} variant="rail" onOpen={vi.fn()} nowMs={NOW} />);
    expect(container.textContent).toBe('Open track');
  });

  it('keeps both on the default variant', () => {
    const { container } = render(
      <TrackRow track={track({ updatedAt: NOW - 3_600_000 })} variant="default" onOpen={vi.fn()} nowMs={NOW} />,
    );
    expect(container.textContent).toContain('Working');
    expect(container.textContent).toContain('1h');
  });

  it('puts the hour label ahead of the title on a panel row, and no relative time after it', () => {
    const { container } = render(<TrackRow track={track()} variant="panel" hourLabel="15:00" onOpen={vi.fn()} nowMs={NOW} />);
    // The whole text of the row: an hour, a title, nothing else. The panel
    // variant drops the age, so a scheduled row states one time, not two.
    expect(container.textContent).toBe('15:00Open track');
  });
});

describe('§2.2 relative time', () => {
  it('floors to one unit and goes absolute past thirty days', () => {
    expect(relativeTime(NOW, NOW)).toBe('now');
    expect(relativeTime(NOW - 90_000, NOW)).toBe('1m');
    expect(relativeTime(NOW - 3 * 3_600_000, NOW)).toBe('3h');
    expect(relativeTime(NOW - 3 * 86_400_000, NOW)).toBe('3d');
    expect(relativeTime(NOW - 9 * 86_400_000, NOW)).toBe('1w');
    // "5w" is not a duration anyone can picture, so it becomes a date.
    expect(relativeTime(NOW - 40 * 86_400_000, NOW)).toMatch(/^[A-Z][a-z]{2} \d+$/);
  });
});
