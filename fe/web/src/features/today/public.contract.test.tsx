// @vitest-environment jsdom
// Invariants for the Today surface. Behavior lives in public.test.tsx.
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import type { Wave } from '../../../../core/domain/wave.ts';
import { TodayPage, type ScheduledEvent } from './public.tsx';

afterEach(cleanup);

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();

function cove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', coveId: 'c1', title: 'Open wave', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...overrides,
  };
}

describe('INV-TODAY-002 the scheduled-event seam', () => {
  it('renders live wave activity while the scheduled list is empty', () => {
    render(<TodayPage waves={[wave()]} coves={[cove()]} onOpenWave={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: /^Wave Open wave/ })).toBeTruthy();
    expect(screen.queryByText('Nothing scheduled.')).toBeNull();
  });

  it('keeps both sources in the same agenda instead of letting either take over', () => {
    // A scheduling plugin has not landed, so production always passes an empty
    // list. Feeding a synthetic event through the seam is the only way to prove
    // the branch still exists and still co-exists with wave activity — deleting
    // it as "dead code" is exactly the regression this locks.
    const scheduled = wave({ id: 'w2', title: 'Scheduled wave', createdAt: NOW - 10 * 86_400_000, terminalAt: NOW - 9 * 86_400_000 });
    const events: ScheduledEvent[] = [{ wave: scheduled, date: new Date(NOW), hour: 15 }];
    render(<TodayPage waves={[wave()]} coves={[cove()]} onOpenWave={vi.fn()} scheduledEvents={events} nowMs={NOW} />);

    expect(screen.getByRole('button', { name: /^Wave Scheduled wave/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /^Wave Open wave/ })).toBeTruthy();
  });

  it('shows the empty state only when both sources are empty', () => {
    render(<TodayPage waves={[]} coves={[cove()]} onOpenWave={vi.fn()} nowMs={NOW} />);
    expect(screen.getByText('Nothing scheduled.')).toBeTruthy();
  });

  it('contributes one dot per wave when both sources carry the same wave', () => {
    const shared = wave({ id: 'w1' });
    const events: ScheduledEvent[] = [{ wave: shared, date: new Date(NOW), hour: 9 }];
    render(<TodayPage waves={[shared]} coves={[cove()]} onOpenWave={vi.fn()} scheduledEvents={events} nowMs={NOW} />);
    const today = screen.getByRole('button', { name: 'Monday, Aug 10' });
    expect(today.querySelectorAll('[data-nc-day-dot]').length).toBe(1);
  });
});

describe('INV-A11Y-061 navigation shape', () => {
  it('routes every agenda row through a button, never a native link', () => {
    const { container } = render(
      <TodayPage waves={[wave()]} coves={[cove()]} onOpenWave={vi.fn()} nowMs={NOW} />,
    );
    expect(container.querySelectorAll('a').length).toBe(0);
    expect(screen.getByRole('button', { name: /^Wave Open wave/ }).tagName).toBe('BUTTON');
  });
});
