// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../core/domain/wave.ts';
import { TodayPage } from './public.tsx';

import type { TodayPageProps } from './public.tsx';

// A stand-in, not the real WaveRow: `features/today` may not import a sibling
// domain, and these suites are about Today's own bucketing and layout. The real
// row has its own tests, and `app/router` is where the two are composed.
const renderWaveRow: TodayPageProps['renderWaveRow'] = (wave, options) => (
  <span data-nc-role="row" data-nc-state={options.variant === 'agenda' ? 'selected' : undefined}>
    {options.hourLabel}{wave.title}
  </span>
);

afterEach(cleanup);

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();
const DAY = 86_400_000;

function cove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', coveId: 'c1', title: 'Open wave', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('Today clock', () => {
  it('counts running and waiting waves with the shared predicates', () => {
    render(<TodayPage renderWaveRow={renderWaveRow} nowMs={NOW} coves={[cove()]} waves={[
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
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[]} coves={[]} nowMs={NOW} />);
    // The page title is the full date — weekday, month, day — in one element.
    expect(screen.getByRole('heading', { name: 'Monday, August 10' })).toBeTruthy();
    // One string, not three elements: the clock is ambient and its whole
    // signal is position, so it spends no structure on itself.
    expect(screen.getByText('3:00 PM')).toBeTruthy();
  });
});

describe('Today agenda', () => {
  // Navigation moved into the injected row (app/router owns the destination),
  // so what Today still owns is *which* wave it hands to the renderer and in
  // which variant. That is what this asserts.
  it('hands each agenda wave to the injected renderer in the agenda variant', () => {
    const seen: { id: string; variant: string }[] = [];
    render(<TodayPage
      renderWaveRow={(candidate, options) => {
        seen.push({ id: candidate.id, variant: options.variant });
        return <span>{candidate.title}</span>;
      }}
      waves={[wave()]} coves={[cove()]} nowMs={NOW}
    />);
    expect(seen.some((entry) => entry.id === 'w1' && entry.variant === 'agenda')).toBe(true);
  });

  // Resolving a wave's cove *name* is Today's job — the agenda spans coves, so
  // the row cannot look it up. Composing that name into an accessible label is
  // the row's job, and is asserted against the real row in
  // `features/wave/row/public.test.tsx`. Asserting a rendered label here would
  // only be re-reading the stand-in defined at the top of this file.
  it('resolves each agenda wave cove name for the renderer', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
      renderWaveRow={(candidate, options) => { seen.push(options.coveName); return <span>{candidate.title}</span>; }}
      waves={[wave({ lifecycle: 'blocked' })]} coves={[cove()]} nowMs={NOW}
    />);
    expect(seen).toContain('Work');
  });

  it('falls back to "Unknown cove" when the wave points at a cove we cannot see', () => {
    const seen: (string | undefined)[] = [];
    render(<TodayPage
      renderWaveRow={(candidate, options) => { seen.push(options.coveName); return <span>{candidate.title}</span>; }}
      waves={[wave({ coveId: 'gone' })]} coves={[cove()]} nowMs={NOW}
    />);
    expect(seen).toContain('Unknown cove');
  });

  it('re-scopes the agenda when another day is selected', async () => {
    // Aug 10 2026 is a Monday, so the visible week is Aug 10–16. This wave only
    // overlaps Tuesday; today's open wave ends at `nowMs` and cannot reach it.
    const tomorrowOnly = wave({
      id: 'y', title: 'Tomorrow only',
      createdAt: NOW + DAY - 3_600_000, terminalAt: NOW + DAY + 3_600_000,
    });
    const agenda = () => screen.getByRole('complementary').textContent ?? '';
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[wave(), tomorrowOnly]} coves={[cove()]} nowMs={NOW} />);
    expect(agenda()).not.toContain('Tomorrow only');

    await userEvent.click(screen.getByRole('button', { name: 'Tuesday, Aug 11' }));
    expect(agenda()).toContain('Tomorrow only');
    expect(agenda()).not.toContain('Open wave');
    expect(screen.getByText('Tuesday, Aug 11')).toBeTruthy();
  });

  // A workspace with a cove, not an empty one: with no waves *and* no coves
  // Today renders the first-run hero instead, and there is no calendar to move.
  it('moves the week window with the previous/next controls', async () => {
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[]} coves={[cove()]} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: 'Previous week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 3' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Next week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });
});
