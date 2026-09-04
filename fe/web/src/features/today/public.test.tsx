// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { TodayPage } from './public.tsx';

import type { TodayPageProps } from './public.tsx';

// A stand-in, not the real TrackRow: `features/today` may not import a sibling
// domain, and these suites are about Today's own bucketing and layout. The real
// row has its own tests, and `app/router` is where the two are composed.
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
  it('counts running and waiting tracks with the shared predicates', () => {
    render(<TodayPage renderTrackRow={renderTrackRow} nowMs={NOW} areas={[area()]} tracks={[
      track({ id: 'a', lifecycle: 'working' }),
      track({ id: 'b', lifecycle: 'planning' }),
      track({ id: 'c', lifecycle: 'blocked' }),
      track({ id: 'd', lifecycle: 'done' }),
    ]} />);
    // The counts are two elements each — a value and a word — because the
    // number takes the weight and the word stays quiet (§3.2 rule 1). So the
    // assertion reads the region, not a single text node.
    expect(screen.getByRole('banner').textContent).toContain('1waiting');
    expect(screen.getByRole('banner').textContent).toContain('2running');
  });

  it('renders the pinned time instead of the wall clock when nowMs is given', () => {
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW} />);
    // The page title is the full date — weekday, month, day — in one element.
    expect(screen.getByRole('heading', { name: 'Monday, August 10' })).toBeTruthy();
    // One string, not three elements: the clock is ambient and its whole
    // signal is position, so it spends no structure on itself.
    expect(screen.getByText('3:00 PM')).toBeTruthy();
  });

  it('moves the page date across midnight on the clock tick', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 7, 10, 23, 59, 50));
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[]} areas={[]} />);
    expect(screen.getByRole('heading', { name: 'Monday, August 10' })).toBeTruthy();

    await act(() => vi.advanceTimersByTime(15_000));
    expect(screen.getByRole('heading', { name: 'Tuesday, August 11' })).toBeTruthy();
  });
});

/*
 * The week grid's label names the week, and the crossing week is the case that
 * matters: a label reading one month over a grid of two is what put
 * `August 2026` on the same screen as `Thursday, September 3`. A suite that only
 * pinned a within-month week would have stayed green through exactly that bug,
 * so the crossing weeks are the point and the within-month one is the control.
 */
describe('Today calendar label', () => {
  it('names the single month when the week does not cross one', () => {
    // NOW is Monday 10 August 2026, whose week is 10–16 August.
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW} />);
    expect(screen.getByText('August 2026')).toBeTruthy();
  });

  it('names both months on a week that crosses one, agreeing with the page header', () => {
    // Thursday 3 September 2026 — the day owner saw. Its week is 31 Aug – 6 Sep.
    const nowMs = new Date(2026, 8, 3, 15, 0, 0).getTime();
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={nowMs} />);
    expect(screen.getByText('Aug – Sep 2026')).toBeTruthy();
    /* The pair, not just the label: the defect was the contradiction between
       these two elements, so both are read in one test. */
    expect(screen.getByRole('heading', { name: 'Thursday, September 3' })).toBeTruthy();
    expect(screen.queryByText('August 2026')).toBeNull();
  });

  it('prints both years on a week that crosses one', () => {
    // Thursday 31 December 2026 — week 28 Dec 2026 – 3 Jan 2027.
    const nowMs = new Date(2026, 11, 31, 15, 0, 0).getTime();
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={nowMs} />);
    /* `Dec – Jan 2027` would file December under 2027. */
    expect(screen.getByText('Dec 2026 – Jan 2027')).toBeTruthy();
  });
});

describe('Today agenda', () => {
  it('excludes archived tracks from counts, sections, calendar dots, and agenda', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow}
      tracks={[track({ title: 'Archived attention', lifecycle: 'blocked', archivedAt: NOW - DAY })]}
      areas={[area()]}
      nowMs={NOW}
    />);
    expect(screen.getByRole('banner').textContent).toContain('0waiting');
    expect(screen.queryByText('Archived attention')).toBeNull();
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });

  // Navigation moved into the injected row (app/router owns the destination),
  // so what Today still owns is *which* track it hands to the renderer and in
  // which variant. That is what this asserts.
  it('hands each agenda track to the injected renderer in the panel variant', () => {
    const seen: { id: string; variant: string }[] = [];
    render(<TodayPage
      renderTrackRow={(candidate, options) => {
        seen.push({ id: candidate.id, variant: options.variant });
        return <span>{candidate.title}</span>;
      }}
      tracks={[track()]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen.some((entry) => entry.id === 'w1' && entry.variant === 'panel')).toBe(true);
  });

  // Resolving a track's area *name* is Today's job — the agenda spans areas, so
  // the row cannot look it up. Composing that name into an accessible label is
  // the row's job, and is asserted against the real row in
  // `features/track/row/public.test.tsx`. Asserting a rendered label here would
  // only be re-reading the stand-in defined at the top of this file.
  it('resolves each agenda track area name for the renderer', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
      renderTrackRow={(candidate, options) => { seen.push(options.areaName); return <span>{candidate.title}</span>; }}
      tracks={[track({ lifecycle: 'blocked' })]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen).toContain('Work');
  });

  it('falls back to "Unknown area" when the track points at an area we cannot see', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
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
    /* The agenda, not the whole panel: since #1253 the card also carries the
       RUNNING and Conversations modules, so `complementary` no longer means
       "the calendar". The agenda's rows are exactly the ones Today asks for with
       `variant: 'panel'`, which the stand-in at the top of this file marks. */
    const agenda = () => [...document.querySelectorAll('[data-nc-role="row"][data-nc-state="selected"]')]
      .map((row) => row.textContent ?? '').join('');
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[track(), tomorrowOnly]} areas={[area()]} nowMs={NOW} />);
    expect(agenda()).not.toContain('Tomorrow only');

    // The day cell's accessible name carries its count — that is the only route
    // to it for assistive tech, since the superscript beside the date is
    // `aria-hidden`. One track overlaps Tuesday, so the name says so.
    await userEvent.click(screen.getByRole('button', { name: 'Tuesday, Aug 11, 1 track' }));
    expect(agenda()).toContain('Tomorrow only');
    expect(agenda()).not.toContain('Open track');
    expect(screen.getByText('Tuesday, Aug 11')).toBeTruthy();
  });

  it('moves the week window with the previous/next controls', async () => {
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[]} areas={[area()]} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: 'Previous week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 3' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Next week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });
});
