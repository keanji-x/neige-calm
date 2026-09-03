// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../../core/domain/wave.ts';
import { WaveList } from './public.tsx';

afterEach(cleanup);

function area(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', areaId: 'c1', title: 'Alpha', sort: 1, lifecycle: 'done', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

function titlesInOrder(): string[] {
  return screen.getAllByRole('button', { name: /^Wave / }).map((node) => node.getAttribute('aria-label') ?? '');
}

describe('WaveList', () => {
  it('renders the empty message when there are no waves', () => {
    render(<WaveList waves={[]} areas={[]} onOpenWave={vi.fn()} emptyMessage="No waves in this area yet." />);
    expect(screen.getByText('No waves in this area yet.')).toBeTruthy();
  });

  it('does not list archived waves', () => {
    render(<WaveList
      waves={[wave({ title: 'Filed away', archivedAt: 42 })]}
      areas={[area()]}
      onOpenWave={vi.fn()}
      emptyMessage="No visible waves."
    />);
    expect(screen.queryByRole('button', { name: /Filed away/ })).toBeNull();
    expect(screen.getByText('No visible waves.')).toBeTruthy();
  });

  it('orders waiting before running before quiet', () => {
    render(<WaveList
      waves={[
        wave({ id: 'q', title: 'Quiet', lifecycle: 'done' }),
        wave({ id: 'r', title: 'Running', lifecycle: 'working' }),
        wave({ id: 'a', title: 'Attention', lifecycle: 'blocked' }),
      ]}
      areas={[area()]}
      onOpenWave={vi.fn()}
      emptyMessage="empty"
    />);
    expect(titlesInOrder().map((label) => label.split(',')[0])).toEqual([
      'Wave Attention', 'Wave Running', 'Wave Quiet',
    ]);
  });

  it('promotes a wave whose card needs input even when its lifecycle is quiet', () => {
    render(<WaveList
      waves={[
        wave({ id: 'r', title: 'Running', lifecycle: 'working' }),
        wave({ id: 'n', title: 'Needy', lifecycle: 'draft', anyCardNeedsInput: true }),
      ]}
      areas={[area()]}
      onOpenWave={vi.fn()}
      emptyMessage="empty"
    />);
    expect(titlesInOrder()[0]?.startsWith('Wave Needy')).toBe(true);
  });

  it('opens the wave the row stands for', async () => {
    const onOpenWave = vi.fn();
    render(<WaveList waves={[wave()]} areas={[area()]} onOpenWave={onOpenWave} emptyMessage="empty" />);
    await userEvent.click(screen.getByRole('button', { name: /^Wave Alpha/ }));
    expect(onOpenWave).toHaveBeenCalledWith('w1');
  });

  it('names the area only when showArea is set', () => {
    const { unmount } = render(
      <WaveList waves={[wave()]} areas={[area()]} onOpenWave={vi.fn()} emptyMessage="empty" />,
    );
    expect(screen.getByRole('button', { name: 'Wave Alpha, Done' })).toBeTruthy();
    unmount();

    render(<WaveList waves={[wave()]} areas={[area()]} showArea onOpenWave={vi.fn()} emptyMessage="empty" />);
    expect(screen.getByRole('button', { name: 'Wave Alpha, Done, in area Work' })).toBeTruthy();
  });

  it('falls back to Unknown area when the wave points at an area we cannot see', () => {
    render(<WaveList
      waves={[wave({ areaId: 'gone' })]}
      areas={[area()]}
      showArea
      onOpenWave={vi.fn()}
      emptyMessage="empty"
    />);
    expect(screen.getByRole('button', { name: /in area Unknown area$/ })).toBeTruthy();
  });

  it('marks the active wave with aria-current', () => {
    render(<WaveList
      waves={[wave({ id: 'w1', title: 'Alpha' }), wave({ id: 'w2', title: 'Beta' })]}
      areas={[area()]}
      activeWaveId="w2"
      onOpenWave={vi.fn()}
      emptyMessage="empty"
    />);
    expect(screen.getByRole('button', { name: /^Wave Beta/ }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('button', { name: /^Wave Alpha/ }).getAttribute('aria-current')).toBeNull();
  });

  it('hides pin and delete unless the callbacks are supplied', () => {
    render(<WaveList waves={[wave()]} areas={[area()]} onOpenWave={vi.fn()} emptyMessage="empty" />);
    expect(screen.queryByRole('button', { name: 'Pin Alpha' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Delete Alpha' })).toBeNull();
  });

  it('forwards pin and delete to the callbacks', async () => {
    const onSetPinned = vi.fn();
    const onDeleteWave = vi.fn();
    render(<WaveList
      waves={[wave()]}
      areas={[area()]}
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
