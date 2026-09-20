// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { TodayPage } from './public.tsx';

import type { TodayPageProps } from './public.tsx';

// A stand-in, not the real TrackRow: `features/today` may not import a sibling domain.
const renderTrackRow: TodayPageProps['renderTrackRow'] = (track, options) => (
  <span data-nc-role="row" data-nc-state={options.variant === 'panel' ? 'selected' : undefined}>
    {options.hourLabel}{track.title}
  </span>
);

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();
const DAY = 86_400_000;

function area(overrides: Partial<Area> = {}): Area {
  return {
    id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0, ...overrides,
  };
}

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Open track', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('Today clock', () => {
  it('counts waiting on you by the kernel verdict and In progress by lifecycle phase', () => {
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} nowMs={NOW} areas={[area()]} tracks={[
      track({ id: 'a', title: 'Working phase, idle', lifecycle: 'working', working: false }),
      track({ id: 'b', title: 'Planning phase, idle planner', lifecycle: 'planning', working: false }),
      track({ id: 'c', title: 'Blocked phase, needs input', lifecycle: 'blocked', attention: 'input' }),
      track({ id: 'd', title: 'Done phase, still in flight', lifecycle: 'done', working: true }),
      track({ id: 'e', title: 'Working phase, failed', lifecycle: 'working', attention: 'failed' }),
      track({ id: 'f', title: 'Blocked phase, nothing from the kernel', lifecycle: 'blocked' }),
    ]} />);
    expect(screen.getByRole('banner').textContent).toContain('2waiting on you');
    expect(screen.getByRole('banner').textContent).toContain('2in progress');
    const section = screen.getByRole('heading', { name: 'In progress' }).closest('section')!;
    expect(section.textContent).toContain('Planning phase, idle planner');
    expect(section.textContent).toContain('Working phase, idle');
    expect(section.textContent).not.toContain('Done phase, still in flight');
    expect(section.textContent).not.toContain('Working phase, failed');
    expect(section.textContent).not.toContain('needs input');
    expect(screen.queryByText('Running')).toBeNull();
  });

  it('renders the pinned time instead of the wall clock when nowMs is given', () => {
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW} />);
    expect(screen.getByRole('heading', { name: 'Monday, August 10' })).toBeTruthy();
    expect(screen.getByText('3:00 PM')).toBeTruthy();
  });

  it('moves the page date across midnight on the clock tick', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 7, 10, 23, 59, 50));
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[]} areas={[]} />);
    expect(screen.getByRole('heading', { name: 'Monday, August 10' })).toBeTruthy();

    await act(() => vi.advanceTimersByTime(15_000));
    expect(screen.getByRole('heading', { name: 'Tuesday, August 11' })).toBeTruthy();
  });
});

describe('Today calendar label', () => {
  it('names the single month when the week does not cross one', () => {
    // NOW is Monday 10 August 2026, whose week is 10–16 August.
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW} />);
    expect(screen.getByText('August 2026')).toBeTruthy();
  });

  it('names both months on a week that crosses one, agreeing with the page header', () => {
    // Thursday 3 September 2026; its week is 31 Aug – 6 Sep.
    const nowMs = new Date(2026, 8, 3, 15, 0, 0).getTime();
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={nowMs} />);
    expect(screen.getByText('Aug – Sep 2026')).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Thursday, September 3' })).toBeTruthy();
    expect(screen.queryByText('August 2026')).toBeNull();
  });

  it('prints both years on a week that crosses one', () => {
    // Thursday 31 December 2026 — week 28 Dec 2026 – 3 Jan 2027.
    const nowMs = new Date(2026, 11, 31, 15, 0, 0).getTime();
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={nowMs} />);
    /* `Dec – Jan 2027` would file December under 2027. */
    expect(screen.getByText('Dec 2026 – Jan 2027')).toBeTruthy();
  });
});

describe('Today agenda', () => {
  it('excludes archived tracks from counts, sections, calendar dots, and agenda', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow}
      tracks={[track({ title: 'Archived attention', lifecycle: 'blocked', archivedAt: NOW - DAY })]}
      areas={[area()]}
      nowMs={NOW}
    />);
    expect(screen.getByRole('banner').textContent).toContain('0waiting on you');
    expect(screen.queryByText('Archived attention')).toBeNull();
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });

  it('hands each agenda track to the injected renderer in the panel variant', () => {
    const seen: { id: string; variant: string }[] = [];
    render(<TodayPage
      activityAvailable
      renderTrackRow={(candidate, options) => {
        seen.push({ id: candidate.id, variant: options.variant });
        return <span>{candidate.title}</span>;
      }}
      tracks={[track()]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen.some((entry) => entry.id === 'w1' && entry.variant === 'panel')).toBe(true);
  });

  it('resolves each agenda track area name for the renderer', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
      activityAvailable
      renderTrackRow={(candidate, options) => { seen.push(options.areaName); return <span>{candidate.title}</span>; }}
      tracks={[track({ lifecycle: 'blocked' })]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen).toContain('Work');
  });

  it('falls back to "Unknown area" when the track points at an area we cannot see', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
      activityAvailable
      renderTrackRow={(candidate, options) => { seen.push(options.areaName); return <span>{candidate.title}</span>; }}
      tracks={[track({ areaId: 'gone' })]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen).toContain('Unknown area');
  });

  it('re-scopes the agenda when another day is selected', async () => {
    // Aug 10 2026 is a Monday, so the visible week is Aug 10–16. This track only
    // overlaps Tuesday; today's open track ends at `nowMs` and cannot reach it.
    const tomorrowOnly = track({
      id: 'y', title: 'Tomorrow only',
      createdAt: NOW + DAY - 3_600_000, terminalAt: NOW + DAY + 3_600_000,
    });
    /* The agenda's rows are exactly the ones Today asks for with `variant: 'panel'`, which the stand-in marks. */
    const agenda = () => [...document.querySelectorAll('[data-nc-role="row"][data-nc-state="selected"]')]
      .map((row) => row.textContent ?? '').join('');
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track(), tomorrowOnly]} areas={[area()]} nowMs={NOW} />);
    expect(agenda()).not.toContain('Tomorrow only');

    await userEvent.click(screen.getByRole('button', { name: 'Tuesday, Aug 11, 1 track' }));
    expect(agenda()).toContain('Tomorrow only');
    expect(agenda()).not.toContain('Open track');
    expect(screen.getByText('Tuesday, Aug 11')).toBeTruthy();
  });

  it('moves the week window with the previous/next controls', async () => {
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[]} areas={[area()]} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: 'Previous week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 3' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Next week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });
});
