// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../../core/domain/cove.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../../core/domain/wave.ts';
import { WaveList } from './public.tsx';

afterEach(cleanup);

function cove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', coveId: 'c1', title: 'Alpha', sort: 1, lifecycle: 'done', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

function titlesInOrder(): string[] {
  return screen.getAllByRole('button', { name: /^Track / }).map((node) => node.getAttribute('aria-label') ?? '');
}

describe('WaveList', () => {
  it('renders the empty message when there are no waves', () => {
    render(<WaveList waves={[]} coves={[]} onOpenWave={vi.fn()} emptyMessage="No tracks in this area yet." />);
    expect(screen.getByText('No tracks in this area yet.')).toBeTruthy();
  });

  it('does not list archived waves', () => {
    render(<WaveList
      waves={[wave({ title: 'Filed away', archivedAt: 42 })]}
      coves={[cove()]}
      onOpenWave={vi.fn()}
      emptyMessage="No visible tracks."
    />);
    expect(screen.queryByRole('button', { name: /Filed away/ })).toBeNull();
    expect(screen.getByText('No visible tracks.')).toBeTruthy();
  });

  it('orders waiting before running before quiet', () => {
    render(<WaveList
      waves={[
        wave({ id: 'q', title: 'Quiet', lifecycle: 'done' }),
        wave({ id: 'r', title: 'Running', lifecycle: 'working' }),
        wave({ id: 'a', title: 'Attention', lifecycle: 'blocked' }),
      ]}
      coves={[cove()]}
      onOpenWave={vi.fn()}
      emptyMessage="empty"
    />);
    expect(titlesInOrder().map((label) => label.split(',')[0])).toEqual([
      'Track Attention', 'Track Running', 'Track Quiet',
    ]);
  });

  it('promotes a wave whose card needs input even when its lifecycle is quiet', () => {
    render(<WaveList
      waves={[
        wave({ id: 'r', title: 'Running', lifecycle: 'working' }),
        wave({ id: 'n', title: 'Needy', lifecycle: 'draft', anyCardNeedsInput: true }),
      ]}
      coves={[cove()]}
      onOpenWave={vi.fn()}
      emptyMessage="empty"
    />);
    expect(titlesInOrder()[0]?.startsWith('Track Needy')).toBe(true);
  });

  it('opens the wave the row stands for', async () => {
    const onOpenWave = vi.fn();
    render(<WaveList waves={[wave()]} coves={[cove()]} onOpenWave={onOpenWave} emptyMessage="empty" />);
    await userEvent.click(screen.getByRole('button', { name: /^Track Alpha/ }));
    expect(onOpenWave).toHaveBeenCalledWith('w1');
  });

  it('names the cove only when showCove is set', () => {
    const { unmount } = render(
      <WaveList waves={[wave()]} coves={[cove()]} onOpenWave={vi.fn()} emptyMessage="empty" />,
    );
    expect(screen.getByRole('button', { name: 'Track Alpha, Done' })).toBeTruthy();
    unmount();

    render(<WaveList waves={[wave()]} coves={[cove()]} showCove onOpenWave={vi.fn()} emptyMessage="empty" />);
    expect(screen.getByRole('button', { name: 'Track Alpha, Done, in area Work' })).toBeTruthy();
  });

  it('falls back to Unknown area when the wave points at a cove we cannot see', () => {
    render(<WaveList
      waves={[wave({ coveId: 'gone' })]}
      coves={[cove()]}
      showCove
      onOpenWave={vi.fn()}
      emptyMessage="empty"
    />);
    expect(screen.getByRole('button', { name: /in area Unknown area$/ })).toBeTruthy();
  });

  it('marks the active wave with aria-current', () => {
    render(<WaveList
      waves={[wave({ id: 'w1', title: 'Alpha' }), wave({ id: 'w2', title: 'Beta' })]}
      coves={[cove()]}
      activeWaveId="w2"
      onOpenWave={vi.fn()}
      emptyMessage="empty"
    />);
    expect(screen.getByRole('button', { name: /^Track Beta/ }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('button', { name: /^Track Alpha/ }).getAttribute('aria-current')).toBeNull();
  });

  it('hides pin and delete unless the callbacks are supplied', () => {
    render(<WaveList waves={[wave()]} coves={[cove()]} onOpenWave={vi.fn()} emptyMessage="empty" />);
    expect(screen.queryByRole('button', { name: 'Pin Alpha' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Delete Alpha' })).toBeNull();
  });

  it('forwards pin and delete to the callbacks', async () => {
    const onSetPinned = vi.fn();
    const onDeleteWave = vi.fn();
    render(<WaveList
      waves={[wave()]}
      coves={[cove()]}
      onOpenWave={vi.fn()}
      onSetPinned={onSetPinned}
      onDeleteWave={onDeleteWave}
      emptyMessage="empty"
    />);
    await userEvent.click(screen.getByRole('button', { name: 'Pin Alpha' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete Alpha' }));
    expect(onSetPinned).toHaveBeenCalledWith('w1', true);
    expect(onDeleteWave).toHaveBeenCalledWith('w1');
  });
});
