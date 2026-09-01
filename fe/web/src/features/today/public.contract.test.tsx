// @vitest-environment jsdom
// Invariants for the Today surface. Behavior lives in public.test.tsx.
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../core/domain/wave.ts';
import { TodayPage, type ScheduledEvent } from './public.tsx';

import type { TodayPageProps } from './public.tsx';

// A stand-in, not the real WaveRow: `features/today` may not import a sibling
// domain, and these suites are about Today's own bucketing and layout. The real
// row has its own tests, and `app/router` is where the two are composed.
const renderWaveRow: TodayPageProps['renderWaveRow'] = (wave, options) => (
  <span data-nc-role="row" data-nc-state={options.variant === 'panel' ? 'selected' : undefined}>
    {options.hourLabel}{wave.title}
  </span>
);

afterEach(cleanup);

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();

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

describe('INV-TODAY-002 the scheduled-event seam', () => {
  it('renders live wave activity while the scheduled list is empty', () => {
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[wave()]} coves={[cove()]} nowMs={NOW} />);
    expect(screen.getByRole('complementary').textContent).toContain('Open wave');
    expect(screen.queryByText('Nothing scheduled.')).toBeNull();
  });

  it('keeps both sources in the same agenda instead of letting either take over', () => {
    // A scheduling plugin has not landed, so production always passes an empty
    // list. Feeding a synthetic event through the seam is the only way to prove
    // the branch still exists and still co-exists with wave activity — deleting
    // it as "dead code" is exactly the regression this locks.
    const scheduled = wave({ id: 'w2', title: 'Scheduled wave', createdAt: NOW - 10 * 86_400_000, terminalAt: NOW - 9 * 86_400_000 });
    const events: ScheduledEvent[] = [{ wave: scheduled, date: new Date(NOW), hour: 15 }];
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[wave()]} coves={[cove()]} scheduledEvents={events} nowMs={NOW} />);

    const agenda = screen.getByRole('complementary').textContent ?? '';
    expect(agenda).toContain('Scheduled wave');
    expect(agenda).toContain('Open wave');
  });

  it('shows the empty state only when both sources are empty', () => {
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[]} coves={[cove()]} nowMs={NOW} />);
    expect(screen.getByText('Nothing scheduled.')).toBeTruthy();
  });

  it('counts a wave once when both sources carry it', () => {
    // The day cell shows how many waves a day holds, so double-counting is the
    // failure this locks: one wave present in both sources must read as 1.
    const shared = wave({ id: 'w1' });
    const events: ScheduledEvent[] = [{ wave: shared, date: new Date(NOW), hour: 9 }];
    render(<TodayPage renderWaveRow={renderWaveRow} waves={[shared]} coves={[cove()]} scheduledEvents={events} nowMs={NOW} />);
    // Both the drawn glyph and the accessible name say one, not two.
    const today = screen.getByRole('button', { name: 'Monday, Aug 10, 1 track' });
    expect(today.querySelector('[data-nc-day-count]')?.textContent).toBe('1');
  });
});

describe('INV-A11Y-061 navigation shape', () => {
  /*
   * Today's half of this invariant, and only its half: it emits no native link
   * of its own — day cells, week arrows and section labels are all buttons or
   * inert text.
   *
   * The *rows* are not asserted here on purpose. Today no longer renders one;
   * it injects a renderer, and `features/**` may not import a sibling domain,
   * so anything this file could render is a stand-in defined at the top of this
   * file. A test that builds a button and then asserts a button is proof of
   * nothing. The row's own shape is locked against the real component in
   * `features/wave/row/public.test.tsx`.
   */
  it('emits no native link anywhere on the surface', () => {
    const { container } = render(
      <TodayPage renderWaveRow={renderWaveRow} waves={[wave()]} coves={[cove()]} nowMs={NOW} />,
    );
    expect(container.querySelectorAll('a').length).toBe(0);
  });
});
