// @vitest-environment jsdom
// INV-DUP-009's one row, tested directly.
//
// Two surfaces render it and neither should retest its internals: the rail
// composes it through `app/shell`, and Today receives it by injection because
// `features/**` may not import a sibling domain. Their suites therefore use
// stand-ins, and a stand-in cannot prove the row is a button, carries a
// composed accessible name, or keeps its pin reachable. This file is where
// those live — against the real component.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { NEUTRAL_ACTIVITY, type Track } from '../../../../../core/domain/track.ts';
import { TrackRow, relativeTime } from './public.tsx';

afterEach(cleanup);

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Open track', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('INV-A11Y-061 navigation shape', () => {
  it('is a button and emits no native link', () => {
    const { container } = render(<TrackRow track={track()} onOpen={vi.fn()} nowMs={NOW} />);
    expect(container.querySelectorAll('a').length).toBe(0);
    expect(screen.getByRole('button', { name: /^Track Open track/ }).tagName).toBe('BUTTON');
  });

  it('opens through the callback, never a href', async () => {
    const onOpen = vi.fn();
    render(<TrackRow track={track()} onOpen={onOpen} nowMs={NOW} />);
    await userEvent.click(screen.getByRole('button', { name: /^Track Open track/ }));
    expect(onOpen).toHaveBeenCalledWith('w1');
  });
});

describe('accessible name', () => {
  // The status dot is `aria-hidden` decoration — it is the *name* that has to
  // carry lifecycle and attention, on every variant, including the rail where
  // the dot is the only thing a sighted user sees.
  it('names the lifecycle, and the attention state when there is one', () => {
    render(<TrackRow track={track({ lifecycle: 'blocked', attention: 'input' })} areaName="Work" onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', {
      name: 'Track Open track, waiting on you, Blocked, in area Work',
    })).toBeTruthy();
  });

  it('names a broken track as needing attention', () => {
    render(<TrackRow track={track({ lifecycle: 'done', attention: 'failed' })} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Track Open track, needs attention, Done' })).toBeTruthy();
  });

  it('names the area only when the surface supplies one', () => {
    render(<TrackRow track={track({ working: true })} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Track Open track, working, Working' })).toBeTruthy();
  });

  it('uses the untitled label rather than an empty name', () => {
    render(<TrackRow track={track({ title: '   ' })} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: /^Track Untitled track/ })).toBeTruthy();
  });
});

describe('INV-SIDEBAR-012 the pin is always reachable, and names its action', () => {
  it('carries aria-pressed in both states', async () => {
    const onSetPinned = vi.fn();
    const view = render(<TrackRow track={track()} onOpen={vi.fn()} onSetPinned={onSetPinned} nowMs={NOW} />);
    const pin = screen.getByRole('button', { name: 'Pin Open track' });
    expect(pin.getAttribute('aria-pressed')).toBe('false');
    await userEvent.click(pin);
    expect(onSetPinned).toHaveBeenCalledWith('w1', true);

    view.rerender(<TrackRow track={track({ pinnedAt: 10 })} onOpen={vi.fn()} onSetPinned={onSetPinned} nowMs={NOW} />);
    expect(screen.getByRole('button', { name: 'Unpin Open track' }).getAttribute('aria-pressed')).toBe('true');
  });

  it('keeps the arrow-up icon while the accessible action changes', () => {
    const view = render(<TrackRow track={track()} onOpen={vi.fn()} onSetPinned={vi.fn()} nowMs={NOW} />);
    const before = screen.getByRole('button', { name: 'Pin Open track' }).querySelector('svg');
    expect(before).toBeTruthy();
    view.rerender(<TrackRow track={track({ pinnedAt: 10 })} onOpen={vi.fn()} onSetPinned={vi.fn()} nowMs={NOW} />);
    const after = screen.getByRole('button', { name: 'Unpin Open track' }).querySelector('svg');
    const expectedPaths = ['M8 12.5V3.5', 'M4 7.5 8 3.5l4 4'];
    expect([...before!.querySelectorAll('path')].map((path) => path.getAttribute('d'))).toEqual(expectedPaths);
    expect([...after!.querySelectorAll('path')].map((path) => path.getAttribute('d'))).toEqual(expectedPaths);
  });

  it('renders no pin and no delete unless the surface supplies the callback', () => {
    render(<TrackRow track={track()} onOpen={vi.fn()} nowMs={NOW} />);
    expect(screen.queryByRole('button', { name: /^(Pin|Unpin|Delete) / })).toBeNull();
  });
});

describe('§6.3 variants differ in what they render, not in what they are', () => {
  it('drops the lifecycle line and the relative time in the rail', () => {
    const { container } = render(<TrackRow track={track()} variant="rail" onOpen={vi.fn()} nowMs={NOW} />);
    expect(container.textContent).toBe('Open track');
  });

  it('keeps both on the default variant', () => {
    const { container } = render(
      <TrackRow track={track({ updatedAt: NOW - 3_600_000 })} variant="default" onOpen={vi.fn()} nowMs={NOW} />,
    );
    expect(container.textContent).toContain('Working');
    expect(container.textContent).toContain('1h');
  });

  it('puts the hour label ahead of the title on a panel row, and no relative time after it', () => {
    const { container } = render(<TrackRow track={track()} variant="panel" hourLabel="15:00" onOpen={vi.fn()} nowMs={NOW} />);
    // The whole text of the row: an hour, a title, nothing else. The panel
    // variant drops the age, so a scheduled row states one time, not two.
    expect(container.textContent).toBe('15:00Open track');
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

describe('navigation activity markers', () => {
  it('shows nothing for an idle read track and a blue unread marker for new activity', () => {
    const view = render(<TrackRow track={track({ lifecycle: 'done' })} variant="rail" onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity]')).toBeNull();
    view.rerender(<TrackRow track={track({ lifecycle: 'done' })} variant="rail" unread onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity="unread"]')).toBeTruthy();
    const row = screen.getByRole('button', { name: /^Track Open track/ });
    expect(document.getElementById(row.getAttribute('aria-describedby')!)?.textContent).toBe('Unread updates');
  });

  /*
   * INV-APP-118 — the dot and the accessible name come from the kernel's
   * activity overlay, never from the lifecycle. The two fixtures are chosen
   * to disagree with the lifecycle in both directions, so a `trackActivityState`
   * (or a name) that fell back to `isRunning(lifecycle)` reddens on each:
   * `planning` with nothing in flight is quiet and not "running"; `done` with
   * work still in flight spins and says so.
   */
  it('derives the marker and the name from the activity overlay, not the lifecycle', () => {
    const view = render(<TrackRow track={track({ lifecycle: 'planning', working: false })} variant="rail" onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity]')).toBeNull();
    expect(screen.getByRole('button', { name: /^Track Open track/ }).getAttribute('aria-label')).not.toMatch(/running|working/);

    view.rerender(<TrackRow track={track({ lifecycle: 'done', working: true })} variant="rail" onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity="working"]')).toBeTruthy();
    expect(screen.getByRole('button', { name: /^Track Open track/ }).getAttribute('aria-label')).toBe('Track Open track, working, Done');
  });

  it('gives needs-input precedence over working and unread, and failed over all three', () => {
    const view = render(<TrackRow track={track({ working: true })} unread onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity="working"]')).toBeTruthy();
    view.rerender(<TrackRow track={track({ working: true, attention: 'input' })} unread onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity="attention"]')).toBeTruthy();
    expect(view.container.querySelector('[data-nc-activity="working"]')).toBeNull();
    view.rerender(<TrackRow track={track({ working: true, attention: 'failed' })} unread onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity="failed"]')).toBeTruthy();
    expect(view.container.querySelector('[data-nc-activity="attention"]')).toBeNull();
  });

  it('ignores the retired any_card_needs_input flag', () => {
    const view = render(<TrackRow track={track({ anyCardNeedsInput: true })} variant="rail" onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity]')).toBeNull();
  });

  it.each(['default', 'compact', 'panel', 'rail'] as const)('paints the same state on the %s variant', (variant) => {
    const view = render(<TrackRow track={track({ attention: 'failed' })} variant={variant} onOpen={vi.fn()} />);
    expect(view.container.querySelectorAll('[data-nc-activity="failed"]')).toHaveLength(1);
  });

  /*
   * The description hangs on the FOLDED state, not on the raw receipt (#1722
   * §5.3, the rule the phone row and the conversation row already follow): an
   * unread track that is also working is "working" — the dot says so and the
   * name says so — and is described by nothing; only when the fold lands on
   * `unread` is the row described, in the one vocabulary. A row that read
   * `aria-describedby` off the `unread` prop would say "Unread updates" beside
   * a spinner, which is the receipt said twice over a fact the name already
   * ranks above it.
   */
  it('describes an unread track only in the folded state, never beside a working name', () => {
    const description = () => {
      const id = screen.getByRole('button', { name: /^Track Open track/ }).getAttribute('aria-describedby');
      return id === null ? null : document.getElementById(id)?.textContent ?? null;
    };
    const view = render(<TrackRow track={track({ working: true })} variant="rail" unread onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity="working"]')).toBeTruthy();
    expect(view.container.querySelector('[data-nc-activity="unread"]')).toBeNull();
    expect(screen.getByRole('button', { name: /^Track Open track/ }).getAttribute('aria-label')).toBe('Track Open track, working, Working');
    expect(description()).toBeNull();

    view.rerender(<TrackRow track={track({ lifecycle: 'done' })} variant="rail" unread onOpen={vi.fn()} />);
    expect(view.container.querySelector('[data-nc-activity="unread"]')).toBeTruthy();
    expect(screen.getByRole('button', { name: /^Track Open track/ }).getAttribute('aria-label')).toBe('Track Open track, Done');
    expect(description()).toBe('Unread updates');
  });
});
