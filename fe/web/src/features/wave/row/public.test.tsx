// @vitest-environment jsdom
// INV-DUP-009's one row, tested directly.
//
// Three surfaces render it and none of them can test it: the rail composes it
// through `app/shell`, the cove page and Today receive it by injection because
// `features/**` may not import a sibling domain. Their suites therefore use
// stand-ins, and a stand-in cannot prove the row is a button, carries a
// composed accessible name, or keeps its pin reachable. This file is where
// those live — against the real component.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { NEUTRAL_ACTIVITY, type Wave } from '../../../../../core/domain/wave.ts';
import { WaveRow, relativeTime } from './public.tsx';

afterEach(cleanup);

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();

function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', coveId: 'c1', title: 'Open wave', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('INV-A11Y-061 navigation shape', () => {
  it('is a button and emits no native link', () => {
    const { container } = render(<WaveRow wave={wave()} onOpen={vi.fn()} nowMs={NOW} />);
    expect(container.querySelectorAll('a').length).toBe(0);
    expect(screen.getByRole('button', { name: /^Wave Open wave/ }).tagName).toBe('BUTTON');
  });

  it('opens through the callback, never a href', async () => {
    const onOpen = vi.fn();
    render(<WaveRow wave={wave()} onOpen={onOpen} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: /^Wave Open wave/ }));
    expect(onOpen).toHaveBeenCalledWith('w1');
  });
});

describe('accessible name', () => {
  // The status dot is `aria-hidden` decoration — it is the *name* that has to
  // carry lifecycle and attention, on every variant, including the rail where
  // the dot is the only thing a sighted user sees.
  it('names the lifecycle, and the attention state when there is one', () => {
    render(<WaveRow wave={wave({ lifecycle: 'blocked' })} coveName="Work" onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', {
      name: 'Wave Open wave, waiting on you, Blocked, in cove Work',
    })).toBeTruthy();
  });

  it('names the cove only when the surface supplies one', () => {
    render(<WaveRow wave={wave()} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Wave Open wave, running, Working' })).toBeTruthy();
  });

  it('uses the untitled label rather than an empty name', () => {
    render(<WaveRow wave={wave({ title: '   ' })} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: /^Wave Untitled wave/ })).toBeTruthy();
  });
});

describe('INV-SIDEBAR-012 the pin is always reachable, and names its action', () => {
  it('carries aria-pressed in both states', async () => {
    const onSetPinned = vi.fn();
    const view = render(<WaveRow wave={wave()} onOpen={vi.fn()} onSetPinned={onSetPinned} nowMs={NOW} />);
    const pin = screen.getByRole('button', { name: 'Pin Open wave' });
    expect(pin.getAttribute('aria-pressed')).toBe('false');
    await userEvent.click(pin);
    expect(onSetPinned).toHaveBeenCalledWith('w1', true);

    view.rerender(<WaveRow wave={wave({ pinnedAt: 10 })} onOpen={vi.fn()} onSetPinned={onSetPinned} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Unpin Open wave' }).getAttribute('aria-pressed')).toBe('true');
  });

  // The glyph names the action, so it flips: ↑ offers "lift this to the top",
  // ↓ offers "put it back". Direction is the whole signal — no hollow/solid
  // pair, no weight change, no colour.
  it('flips the arrow once the wave is pinned', () => {
    const view = render(<WaveRow wave={wave()} onOpen={vi.fn()} onSetPinned={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Pin Open wave' }).textContent).toBe('↑');
    view.rerender(<WaveRow wave={wave({ pinnedAt: 10 })} onOpen={vi.fn()} onSetPinned={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Unpin Open wave' }).textContent).toBe('↓');
  });

  it('renders no pin and no delete unless the surface supplies the callback', () => {
    render(<WaveRow wave={wave()} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.queryByRole('button', { name: /^(Pin|Unpin|Delete) / })).toBeNull();
  });
});

describe('§6.3 variants differ in what they render, not in what they are', () => {
  it('drops the lifecycle line and the relative time in the rail', () => {
    const { container } = render(<WaveRow wave={wave()} variant="rail" onOpen={vi.fn()} nowMs={NOW} />);
    expect(container.textContent).toBe('Open wave');
  });

  it('keeps both on the default variant', () => {
    const { container } = render(
      <WaveRow wave={wave({ updatedAt: NOW - 3_600_000 })} variant="default" onOpen={vi.fn()} nowMs={NOW} />,
    );
    expect(container.textContent).toContain('Working');
    expect(container.textContent).toContain('1h');
  });

  it('puts the hour label ahead of the title on a panel row, and no relative time after it', () => {
    const { container } = render(<WaveRow wave={wave()} variant="panel" hourLabel="15:00" onOpen={vi.fn()} nowMs={NOW} />);
    // The whole text of the row: an hour, a title, nothing else. The panel
    // variant drops the age, so a scheduled row states one time, not two.
    expect(container.textContent).toBe('15:00Open wave');
  });
});

describe('§2.2 relative time', () => {
  it('floors to one unit and goes absolute past thirty days', () => {
    expect(relativeTime(NOW, NOW)).toBe('now');
    expect(relativeTime(NOW - 90_000, NOW)).toBe('1m');
    expect(relativeTime(NOW - 3 * 3_600_000, NOW)).toBe('3h');
    expect(relativeTime(NOW - 3 * 86_400_000, NOW)).toBe('3d');
    expect(relativeTime(NOW - 9 * 86_400_000, NOW)).toBe('1w');
    // "5w" is not a duration anyone can picture, so it becomes a date.
    expect(relativeTime(NOW - 40 * 86_400_000, NOW)).toMatch(/^[A-Z][a-z]{2} \d+$/);
  });
});
