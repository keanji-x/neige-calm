// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../core/domain/wave.ts';
import { TodayPage } from './public.tsx';

import type { TodayPageProps } from './public.tsx';

// A stand-in, not the real WaveRow: `features/today` may not import a sibling
// domain, and these suites are about Today's own bucketing and layout. The real
// row has its own tests, and `app/router` is where the two are composed.
const renderWaveRow: TodayPageProps['renderWaveRow'] = (wave, options) => (
  <span data-nc-role="row" data-nc-state={options.variant === 'panel' ? 'selected' : undefined}>
    {options.hourLabel}{wave.title}
  </span>
);

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();
const DAY = 86_400_000;

function area(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', areaId: 'c1', title: 'Open wave', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('Today clock', () => {
  it('counts running and waiting waves with the shared predicates', () => {
    render(<TodayPage renderWaveRow={renderWaveRow} nowMs={NOW} areas={[area()]} waves={[
      wave({ id: 'a', lifecycle: 'working' }),
      wave({ id: 'b', lifecycle: 'planning' }),
      wave({ id: 'c', lifecycle: 'blocked' }),
      wave({ id: 'd', lifecycle: 'done' }),
    ]} />);
    // The counts are two elements each — a value and a word — because the
    // number takes the weight and the word stays quiet (§3.2 rule 1). So the
    // assertion reads the region, not a single text node.
    expect(screen.getByRole('banner').textContent).toContain('1waiting');
    expect(screen.getByRole('banner').textContent).toContain('2running');
  });

  it('renders the pinned time instead of the wall clock when nowMs is given', () => {
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[]} areas={[]} nowMs={NOW} />);
    // The page title is the full date — weekday, month, day — in one element.
    expect(screen.getByRole('heading', { name: 'Monday, August 10' })).toBeTruthy();
    // One string, not three elements: the clock is ambient and its whole
    // signal is position, so it spends no structure on itself.
    expect(screen.getByText('3:00 PM')).toBeTruthy();
  });

  it('moves the page date across midnight on the clock tick', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 7, 10, 23, 59, 50));
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[]} areas={[]} />);
    expect(screen.getByRole('heading', { name: 'Monday, August 10' })).toBeTruthy();

    await act(() => vi.advanceTimersByTime(15_000));
    expect(screen.getByRole('heading', { name: 'Tuesday, August 11' })).toBeTruthy();
  });
});

describe('Today agenda', () => {
  it('excludes archived waves from counts, sections, calendar dots, and agenda', () => {
    render(<TodayPage
      renderWaveRow={renderWaveRow}
      waves={[wave({ title: 'Archived attention', lifecycle: 'blocked', archivedAt: NOW - DAY })]}
      areas={[area()]}
      nowMs={NOW}
    />);
    expect(screen.getByRole('banner').textContent).toContain('0waiting');
    expect(screen.queryByText('Archived attention')).toBeNull();
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });

  // Navigation moved into the injected row (app/router owns the destination),
  // so what Today still owns is *which* wave it hands to the renderer and in
  // which variant. That is what this asserts.
  it('hands each agenda wave to the injected renderer in the panel variant', () => {
    const seen: { id: string; variant: string }[] = [];
    render(<TodayPage
      renderWaveRow={(candidate, options) => {
        seen.push({ id: candidate.id, variant: options.variant });
        return <span>{candidate.title}</span>;
      }}
      waves={[wave()]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen.some((entry) => entry.id === 'w1' && entry.variant === 'panel')).toBe(true);
  });

  // Resolving a wave's area *name* is Today's job — the agenda spans areas, so
  // the row cannot look it up. Composing that name into an accessible label is
  // the row's job, and is asserted against the real row in
  // `features/wave/row/public.test.tsx`. Asserting a rendered label here would
  // only be re-reading the stand-in defined at the top of this file.
  it('resolves each agenda wave area name for the renderer', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
      renderWaveRow={(candidate, options) => { seen.push(options.areaName); return <span>{candidate.title}</span>; }}
      waves={[wave({ lifecycle: 'blocked' })]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen).toContain('Work');
  });

  it('falls back to "Unknown area" when the wave points at an area we cannot see', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
      renderWaveRow={(candidate, options) => { seen.push(options.areaName); return <span>{candidate.title}</span>; }}
      waves={[wave({ areaId: 'gone' })]} areas={[area()]} nowMs={NOW}
    />);
    expect(seen).toContain('Unknown area');
  });

  it('re-scopes the agenda when another day is selected', async () => {
    // Aug 10 2026 is a Monday, so the visible week is Aug 10–16. This wave only
    // overlaps Tuesday; today's open wave ends at `nowMs` and cannot reach it.
    const tomorrowOnly = wave({
      id: 'y', title: 'Tomorrow only',
      createdAt: NOW + DAY - 3_600_000, terminalAt: NOW + DAY + 3_600_000,
    });
    /* The agenda, not the whole panel: since #1253 the card also carries the
       RUNNING and RECENT modules, so `complementary` no longer means "the
       calendar". The agenda's rows are exactly the ones Today asks for with
       `variant: 'panel'`, which the stand-in at the top of this file marks. */
    const agenda = () => [...document.querySelectorAll('[data-nc-role="row"][data-nc-state="selected"]')]
      .map((row) => row.textContent ?? '').join('');
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[wave(), tomorrowOnly]} areas={[area()]} nowMs={NOW} />);
    expect(agenda()).not.toContain('Tomorrow only');

    // The day cell's accessible name carries its count — that is the only route
    // to it for assistive tech, since the superscript beside the date is
    // `aria-hidden`. One wave overlaps Tuesday, so the name says so.
    await userEvent.click(screen.getByRole('button', { name: 'Tuesday, Aug 11, 1 wave' }));
    expect(agenda()).toContain('Tomorrow only');
    expect(agenda()).not.toContain('Open wave');
    expect(screen.getByText('Tuesday, Aug 11')).toBeTruthy();
  });

  // A workspace with an area, not an empty one: with no waves *and* no areas
  // Today renders the first-run hero instead, and there is no calendar to move.
  it('moves the week window with the previous/next controls', async () => {
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[]} areas={[area()]} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: 'Previous week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 3' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Next week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });
});
