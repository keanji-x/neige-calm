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
  <span data-nc-role="row" data-nc-state={options.variant === 'compact' ? 'selected' : undefined}>
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
    id: 'w1', areaId: 'c1', title: 'Open track', sort: 1, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('Today clock', () => {
  /* Both numbers are the kernel's verdicts (`needsUserAttention ∨ hasFailed`, `isWorking`); the
   * "Open" group is every open track not waiting on a person. An open track with an idle planner is open but not working. */
  it('counts waiting on you and working by the kernel verdict, and groups Open by the open state', () => {
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} nowMs={NOW} areas={[area()]} tracks={[
      track({ id: 'a', title: 'Open, idle', working: false }),
      track({ id: 'b', title: 'Open, idle planner', working: false }),
      track({ id: 'c', title: 'Open, needs input', attention: 'input' }),
      track({ id: 'd', title: 'Closed, still in flight', closedAt: NOW - 1, working: true }),
      track({ id: 'e', title: 'Open, failed', attention: 'failed' }),
    ]} />);
    expect(screen.getByRole('banner').textContent).toContain('2waiting on you');
    expect(screen.getByRole('banner').textContent).toContain('1working');
    expect(screen.getByRole('banner').textContent).not.toContain('in progress');
    const section = screen.getByRole('heading', { name: 'Activity' }).closest('section')!;
    expect(section.textContent).toContain('Open, idle planner');
    expect(section.textContent).toContain('Open, idle');
    expect(section.textContent).not.toContain('Closed, still in flight');
    expect(section.textContent).toContain('Open, failed');
    expect(section.textContent).toContain('needs input');
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
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[]} areas={[]}
      renderCalendarTasks={(date) => <div role="status" aria-label="Task date">{date}</div>} />);
    expect(screen.getByRole('status', { name: 'Task date' }).textContent).toBe('2026-08-10');
    expect(screen.getByRole('heading', { name: 'Monday, August 10' })).toBeTruthy();

    await act(() => vi.advanceTimersByTime(15_000));
    expect(screen.getByRole('heading', { name: 'Tuesday, August 11' })).toBeTruthy();
    expect(screen.getByRole('status', { name: 'Task date' }).textContent).toBe('2026-08-11');
  });
});

it('keeps a deliberately selected future day when the clock passes midnight', async () => {
  const props = { tracks: [], areas: [], activityAvailable: true, renderTrackRow,
    renderCalendarTasks: (date: string, onDateChange: (date: string) => void) => <><div role="status" aria-label="Task date">{date}</div><button type="button" onClick={() => onDateChange('2026-08-12')}>Choose August 12</button></> };
  const view = render(<TodayPage {...props} nowMs={new Date(2026, 7, 10, 23, 59, 50).getTime()} />);
  await userEvent.click(screen.getByRole('button', { name: 'Choose August 12' }));
  view.rerender(<TodayPage {...props} nowMs={new Date(2026, 7, 11, 0, 0, 5).getTime()} />);
  expect(screen.getByRole('status', { name: 'Task date' }).textContent).toBe('2026-08-12');
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
  it('orders activity by latest row or overlay update and reorders after an update without mutating inputs', () => {
    const older = track({ id: 'older', title: 'Older update', createdAt: NOW - DAY, updatedAt: NOW - 3_000 });
    const newer = track({ id: 'newer', title: 'Newer update', createdAt: NOW - 3 * DAY, updatedAt: NOW - 1_000 });
    const overlay = track({ id: 'overlay', title: 'Recent activity', createdAt: NOW - 2 * DAY, updatedAt: NOW - 4_000, recentAt: NOW });
    const tracks = Object.freeze([older, newer, overlay]);
    const props = { activityAvailable: true, renderTrackRow, nowMs: NOW, areas: [area()], tracks };
    const view = render(<TodayPage {...props} />);
    const titles = () => [...screen.getByRole('region', { name: 'Activity list' }).querySelectorAll('[data-nc-role="row"]')]
      .map((row) => row.textContent);
    expect(titles()).toEqual(['Recent activity', 'Newer update', 'Older update']);
    expect(tracks.map((candidate) => candidate.id)).toEqual(['older', 'newer', 'overlay']);
    view.rerender(<TodayPage {...props} tracks={[{ ...older, updatedAt: NOW + 1 }, newer, overlay]} />);
    expect(titles()).toEqual(['Older update', 'Recent activity', 'Newer update']);
  });

  it('hands each agenda track to the injected renderer in the compact variant with update time', () => {
    const seen: { id: string; variant: string }[] = [];
    render(<TodayPage
      activityAvailable
      renderTrackRow={(candidate, options) => {
        seen.push({ id: candidate.id, variant: options.variant });
        return <span>{candidate.title}</span>;
      }}
      tracks={[track()]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen.some((entry) => entry.id === 'w1' && entry.variant === 'compact')).toBe(true);
  });

  it('resolves each agenda track area name for the renderer', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
      activityAvailable
      renderTrackRow={(candidate, options) => { seen.push(options.areaName); return <span>{candidate.title}</span>; }}
      tracks={[track()]} areas={[area()]} nowMs={NOW}
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
      createdAt: NOW + DAY - 3_600_000, closedAt: NOW + DAY + 3_600_000,
    });
    /* The agenda's rows are exactly the ones Today asks for with `variant: 'panel'`, which the stand-in marks. */
    const agenda = () => [...document.querySelectorAll('[data-nc-role="row"][data-nc-state="selected"]')]
      .map((row) => row.textContent ?? '').join('');
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track(), tomorrowOnly]} areas={[area()]} nowMs={NOW} />);
    expect(agenda()).not.toContain('Tomorrow only');

    await userEvent.click(screen.getByRole('button', { name: 'Tuesday, Aug 11' }));
    expect(agenda()).not.toContain('Tomorrow only');
    await userEvent.click(screen.getByRole('button', { name: 'Activity filters' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /Show closed/ }));
    expect(agenda()).toContain('Tomorrow only');
    expect(screen.getByRole('button', { name: 'Tuesday, Aug 11, 1 track' })).toBeTruthy();
    expect(agenda()).not.toContain('Open track');
    expect(screen.getByRole('heading', { name: 'Activity' })).toBeTruthy();
  });

  it('moves the week window with the previous/next controls', async () => {
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[]} areas={[area()]} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: 'Previous week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 3' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Next week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });
});

it('filters read tracks using the injected receipt state and keeps unread tracks', async () => {
  render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track({ id: 'read', title: 'Read track' }), track({ id: 'unread', title: 'Unread track' })]} areas={[area()]} nowMs={NOW} isTrackUnread={(value) => value.id === 'unread'} />);
  await userEvent.click(screen.getByRole('button', { name: 'Activity filters' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: /Show read/ }));
  expect(screen.queryByText('Read track')).toBeNull();
  expect(screen.getByText('Unread track')).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Monday, Aug 10, 1 track' })).toBeTruthy();
});
