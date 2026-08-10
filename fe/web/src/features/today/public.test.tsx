// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import type { Wave } from '../../../../core/domain/wave.ts';
import { TodayPage } from './public.tsx';

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
    ...overrides,
  };
}

describe('Today clock', () => {
  it('counts running and waiting waves with the shared predicates', () => {
    render(<TodayPage nowMs={NOW} coves={[cove()]} onOpenWave={vi.fn()} waves={[
      wave({ id: 'a', lifecycle: 'working' }),
      wave({ id: 'b', lifecycle: 'planning' }),
      wave({ id: 'c', lifecycle: 'blocked' }),
      wave({ id: 'd', lifecycle: 'done' }),
    ]} />);
    expect(screen.getByText(/2 running/)).toBeTruthy();
    expect(screen.getByText(/1 waiting/)).toBeTruthy();
  });

  it('renders the pinned time instead of the wall clock when nowMs is given', () => {
    render(<TodayPage waves={[]} coves={[]} onOpenWave={vi.fn()} nowMs={NOW} />);
    expect(screen.getByText('Monday')).toBeTruthy();
    expect(screen.getByText('PM')).toBeTruthy();
  });
});

describe('Today agenda', () => {
  it('opens the wave the row stands for', async () => {
    const onOpenWave = vi.fn();
    render(<TodayPage waves={[wave()]} coves={[cove()]} onOpenWave={onOpenWave} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: /^Wave Open wave/ }));
    expect(onOpenWave).toHaveBeenCalledWith('w1');
  });

  it('names the cove and the lifecycle in the row accessible name', () => {
    render(<TodayPage waves={[wave({ lifecycle: 'blocked' })]} coves={[cove()]} onOpenWave={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', {
      name: 'Wave Open wave, waiting on you, Blocked, in cove Work',
    })).toBeTruthy();
  });

  it('falls back to "Unknown cove" when the wave points at a cove we cannot see', () => {
    render(<TodayPage waves={[wave({ coveId: 'gone' })]} coves={[cove()]} onOpenWave={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: /in cove Unknown cove$/ })).toBeTruthy();
  });

  it('re-scopes the agenda when another day is selected', async () => {
    // Aug 10 2026 is a Monday, so the visible week is Aug 10–16. This wave only
    // overlaps Tuesday; today's open wave ends at `nowMs` and cannot reach it.
    const tomorrowOnly = wave({
      id: 'y', title: 'Tomorrow only',
      createdAt: NOW + DAY - 3_600_000, terminalAt: NOW + DAY + 3_600_000,
    });
    render(<TodayPage waves={[wave(), tomorrowOnly]} coves={[cove()]} onOpenWave={vi.fn()} nowMs={NOW} />);
    expect(screen.queryByRole('button', { name: /^Wave Tomorrow only/ })).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Tuesday, Aug 11' }));
    expect(screen.getByRole('button', { name: /^Wave Tomorrow only/ })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /^Wave Open wave/ })).toBeNull();
    expect(screen.getByText('Tuesday, Aug 11')).toBeTruthy();
  });

  it('moves the week window with the previous/next controls', async () => {
    render(<TodayPage waves={[]} coves={[]} onOpenWave={vi.fn()} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: 'Previous week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 3' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Next week' }));
    expect(screen.getByRole('button', { name: 'Monday, Aug 10' })).toBeTruthy();
  });
});
