// Calendar page tests — issue #250 PR 5.
//
// Two clusters:
//   1. Pure-function tests on `waveSpanInWeek` / `packLanes` — fast,
//      no DOM, lock the layout math against regressions before we even
//      reach React rendering.
//   2. Component tests on `<CalendarPage />` — assert that the 7
//      day-header columns render, that a wave fixture spanning days 0..3
//      occupies exactly columns 1..4, that clicking the bar dispatches
//      a navigate to /wave/:id, and that the week-nav arrows produce a
//      new `onWeekChange` event with an anchor offset by ±7 days.
//
// The component renders with a controlled `weekAnchor` so the tests
// don't have to mock `Date.now()` for the visible week. `nowMs` is
// pinned as well so the "open wave" branch (terminal_at == null)
// extends to a predictable column.

import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import {
  CalendarPage,
  packLanes,
  startOfWeek,
  waveSpanInWeek,
  type CalendarWave,
  type WaveSpan,
} from './Calendar';
import type { Cove, Route } from '../types';

// Fixed anchor: Wed 2024-05-15 12:00 local. The Mon-anchored week is
// 2024-05-13 00:00 .. 2024-05-19 23:59. Picked midweek so a "today"
// column actually highlights inside the rendered grid.
const ANCHOR = new Date(2024, 4, 15, 12, 0, 0, 0).getTime();
const WEEK_START = startOfWeek(new Date(ANCHOR));

function tsOnDay(offset: number, hour = 9): number {
  const d = new Date(WEEK_START);
  d.setDate(d.getDate() + offset);
  d.setHours(hour, 0, 0, 0);
  return d.getTime();
}

const coves: Cove[] = [
  { id: 'cA', name: 'Atlas', subtitle: '', color: '#5a9' },
  { id: 'cB', name: 'Beacon', subtitle: '', color: '#c97' },
];

describe('waveSpanInWeek', () => {
  it('returns null for waves wholly before the week', () => {
    const before = tsOnDay(-10);
    const ended = tsOnDay(-3);
    const out = waveSpanInWeek(before, ended, WEEK_START, ANCHOR);
    expect(out).toBeNull();
  });

  it('returns null for waves wholly after the week', () => {
    const start = tsOnDay(10);
    const end = tsOnDay(12);
    const out = waveSpanInWeek(start, end, WEEK_START, ANCHOR);
    expect(out).toBeNull();
  });

  it('places a wave that occupies columns 0..3 (Mon..Thu)', () => {
    const start = tsOnDay(0, 8);
    const end = tsOnDay(3, 17);
    const out = waveSpanInWeek(start, end, WEEK_START, ANCHOR);
    expect(out).not.toBeNull();
    const span = out as WaveSpan;
    expect(span.start).toBe(0);
    expect(span.end).toBe(3);
    expect(span.clipLeft).toBe(false);
    expect(span.clipRight).toBe(false);
  });

  it('clips left when the wave began before the week', () => {
    const start = tsOnDay(-5);
    const end = tsOnDay(2);
    const out = waveSpanInWeek(start, end, WEEK_START, ANCHOR);
    const span = out as WaveSpan;
    expect(span.start).toBe(0);
    expect(span.end).toBe(2);
    expect(span.clipLeft).toBe(true);
    expect(span.clipRight).toBe(false);
  });

  it('clips right and uses `now` for open (terminal_at = null) waves', () => {
    const start = tsOnDay(1);
    const out = waveSpanInWeek(start, null, WEEK_START, ANCHOR);
    const span = out as WaveSpan;
    // Anchor is on day 2 (Wed); the bar should reach at least column 2.
    expect(span.start).toBe(1);
    expect(span.end).toBeGreaterThanOrEqual(2);
    expect(span.clipRight).toBe(true);
  });

  it('clips right for waves extending past Sun', () => {
    const start = tsOnDay(5);
    const end = tsOnDay(20);
    const out = waveSpanInWeek(start, end, WEEK_START, ANCHOR);
    const span = out as WaveSpan;
    expect(span.start).toBe(5);
    expect(span.end).toBe(6);
    expect(span.clipRight).toBe(true);
  });
});

describe('packLanes', () => {
  const mkItem = (id: string, start: number, end: number) => ({
    id,
    span: { start, end, clipLeft: false, clipRight: false, createdAt: 0, endAt: 0 },
  });

  it('places non-overlapping spans on a single lane', () => {
    const lanes = packLanes([mkItem('a', 0, 1), mkItem('b', 3, 4), mkItem('c', 6, 6)]);
    expect(lanes).toHaveLength(1);
    expect(lanes[0].map((i) => i.id)).toEqual(['a', 'b', 'c']);
  });

  it('opens a second lane when spans overlap', () => {
    const lanes = packLanes([mkItem('a', 0, 3), mkItem('b', 2, 4), mkItem('c', 5, 6)]);
    expect(lanes).toHaveLength(2);
    expect(lanes[0].map((i) => i.id)).toEqual(['a', 'c']);
    expect(lanes[1].map((i) => i.id)).toEqual(['b']);
  });
});

// ------------------------------------------------------------------
// Component tests
// ------------------------------------------------------------------

function makeWaves(): CalendarWave[] {
  return [
    // Cove A — spans Mon..Thu (cols 1..4), Done.
    {
      id: 'wA',
      title: 'Migrate auth',
      coveId: 'cA',
      lifecycle: 'done',
      createdAt: tsOnDay(0, 9),
      terminalAt: tsOnDay(3, 17),
      cwd: '/srv/atlas',
    },
    // Cove B — created on Wed (col 3), still open. Should extend to
    // the "today" column (Wed, col 2 — note ANCHOR is on Wed at 12:00).
    {
      id: 'wB',
      title: 'Spike Beacon plugin',
      coveId: 'cB',
      lifecycle: 'working',
      createdAt: tsOnDay(2, 10),
      terminalAt: null,
      cwd: '/srv/beacon',
    },
  ];
}

describe('<CalendarPage />', () => {
  it('renders 7 day columns headed Mon..Sun', () => {
    render(
      <CalendarPage
        waves={[]}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={() => {}}
      />,
    );
    // The calendar is intentionally not an ARIA grid (see Calendar.tsx
    // for the rationale — bars are real <button>s, layout cells are not
    // a tabular header). We assert the day-header row via its class so
    // the test mirrors the DOM AT actually walks.
    const columnHeaders = document.querySelectorAll('.calendar-col-head');
    expect(columnHeaders).toHaveLength(7);
    // Header text is "Mon 13".."Sun 19" (the week containing 2024-05-15).
    expect(columnHeaders[0].textContent).toMatch(/^Mon\s+13/);
    expect(columnHeaders[6].textContent).toMatch(/^Sun\s+19/);
  });

  it('renders a wave bar for each visible wave with cove-coloured background', () => {
    render(
      <CalendarPage
        waves={makeWaves()}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={() => {}}
      />,
    );
    const barA = screen.getByRole('button', {
      name: /Wave Migrate auth in cove Atlas, done/i,
    });
    const barB = screen.getByRole('button', {
      name: /Wave Spike Beacon plugin in cove Beacon, working, ongoing/i,
    });
    // Cove colour is painted inline.
    expect((barA as HTMLElement).style.background).toContain('rgb(85, 170, 153)');
    expect((barB as HTMLElement).style.background).toContain('rgb(204, 153, 119)');
  });

  it('spans wA across columns 1..4 (Mon..Thu)', () => {
    render(
      <CalendarPage
        waves={makeWaves()}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={() => {}}
      />,
    );
    const barA = screen.getByRole('button', {
      name: /Wave Migrate auth/i,
    }) as HTMLElement;
    // `grid-column` is reflected as inline `grid-column-start` /
    // `grid-column-end` by JSDOM. The Calendar renders the shorthand
    // `gridColumn: '1 / 5'` for a wave on cols 0..3 inclusive.
    expect(barA.style.gridColumn).toBe('1 / 5');
  });

  it('extends an open wave to today (Wed = col 3)', () => {
    render(
      <CalendarPage
        waves={makeWaves()}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={() => {}}
      />,
    );
    const barB = screen.getByRole('button', {
      name: /Wave Spike Beacon plugin/i,
    }) as HTMLElement;
    // Created Wed (col index 2) → grid-column-start = 3. With now on
    // the same day end col = 2 → grid-column-end = 4.
    expect(barB.style.gridColumn).toBe('3 / 4');
  });

  it('dispatches a wave-navigate when a bar is clicked', async () => {
    const onGo = vi.fn<(r: Route) => void>();
    const user = userEvent.setup();
    render(
      <CalendarPage
        waves={makeWaves()}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={onGo}
      />,
    );
    await user.click(
      screen.getByRole('button', { name: /Wave Migrate auth/i }),
    );
    expect(onGo).toHaveBeenCalledWith({ name: 'wave', id: 'wA' });
  });

  it('"Next week" advances the anchor by 7 days via onWeekChange', async () => {
    const onWeekChange = vi.fn<(n: number) => void>();
    const user = userEvent.setup();
    render(
      <CalendarPage
        waves={[]}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={() => {}}
        onWeekChange={onWeekChange}
      />,
    );
    await user.click(screen.getByRole('button', { name: /next week/i }));
    expect(onWeekChange).toHaveBeenCalledTimes(1);
    const newAnchor = onWeekChange.mock.calls[0][0];
    expect(newAnchor - ANCHOR).toBe(7 * 24 * 60 * 60 * 1000);
  });

  it('"Previous week" rewinds the anchor by 7 days', async () => {
    const onWeekChange = vi.fn<(n: number) => void>();
    const user = userEvent.setup();
    render(
      <CalendarPage
        waves={[]}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={() => {}}
        onWeekChange={onWeekChange}
      />,
    );
    await user.click(screen.getByRole('button', { name: /previous week/i }));
    const newAnchor = onWeekChange.mock.calls[0][0];
    expect(ANCHOR - newAnchor).toBe(7 * 24 * 60 * 60 * 1000);
  });

  it('renders an empty-state when no waves fall inside the week', () => {
    render(
      <CalendarPage
        waves={[]}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={() => {}}
      />,
    );
    expect(screen.getByText(/no waves this week/i)).toBeInTheDocument();
  });

  it('exposes wave metadata via the tooltip title attr', () => {
    render(
      <CalendarPage
        waves={makeWaves()}
        coves={coves}
        weekAnchor={ANCHOR}
        nowMs={ANCHOR}
        onGo={() => {}}
      />,
    );
    const barB = screen.getByRole('button', { name: /Wave Spike Beacon plugin/i });
    const title = barB.getAttribute('title') ?? '';
    expect(title).toMatch(/Spike Beacon plugin/);
    expect(title).toMatch(/Cove: Beacon/);
    expect(title).toMatch(/Lifecycle: working/);
    expect(title).toMatch(/cwd: \/srv\/beacon/);
    expect(title).toMatch(/Still open/);
  });
});
