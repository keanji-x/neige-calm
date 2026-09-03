// Tests for the Today page's calendar rail — issue #250 PR 5.
//
// What we lock in here:
//
//   1. `activeTracksOn` honours the inclusive [createdAt, terminalAt ?? now]
//      predicate (open tracks stay active forever; terminated tracks drop
//      off after their terminal day; still-future tracks stay invisible).
//   2. CalWeek paints one area-coloured dot per active track on each
//      day cell, capped at four.
//   3. CalMonth paints up to three area-coloured dots per active day.
//   4. Selecting a day surfaces that day's active tracks in the agenda
//      list (area name + title visible, click navigates to the track).
//   5. A day with zero active tracks and zero scheduled events shows the
//      "Nothing scheduled." empty state.
//
// Today's terminal panel is mocked out (we pass `todayTerminalId={null}`
// so the calm "booting" line renders; the lazy XtermView never mounts).
//
// jsdom + fake timers note: `nowMs` is passed explicitly so the
// "still-open track" branch is deterministic across runs.

import { describe, it, expect, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { TodayPage, activeTracksOn } from './Today';
import type { Area, Track } from '../types';

// A track whose terminal_at is `null` is "still open"; the calendar uses
// `Date.now()` to extend its activity through every day up to today.
// The TodayPage component swallows that via its `nowMs` plumbing — we
// pass an explicit value below so the test doesn't depend on wall
// clock drift.

const PINNED_NOW = Date.UTC(2026, 4, 24, 12, 0, 0); // 2026-05-24 12:00 UTC
const DAY_MS = 24 * 60 * 60 * 1000;

function makeArea(overrides: Partial<Area> = {}): Area {
  return {
    id: 'area-atlas',
    name: 'Atlas',
    subtitle: '',
    color: '#5a9',
    ...overrides,
  };
}

function makeTrack(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1',
    areaId: 'area-atlas',
    title: 'Migrate auth',
    lifecycle: 'working',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    createdAt: PINNED_NOW - 3 * DAY_MS,
    terminalAt: null,
    pinnedAt: null,
    cards: [],
    ...overrides,
  };
}

describe('activeTracksOn', () => {
  const atlas = makeArea();

  it('includes a still-open track on every day from createdAt to now (inclusive)', () => {
    const w = makeTrack({
      createdAt: PINNED_NOW - 2 * DAY_MS,
      terminalAt: null,
    });
    const todayMinus2 = new Date(PINNED_NOW - 2 * DAY_MS);
    const today = new Date(PINNED_NOW);
    const tomorrow = new Date(PINNED_NOW + DAY_MS);

    expect(activeTracksOn([w], todayMinus2, PINNED_NOW).map((x) => x.id)).toEqual(['w1']);
    expect(activeTracksOn([w], today, PINNED_NOW).map((x) => x.id)).toEqual(['w1']);
    // Future days past `now` are NOT included — an open track grows up
    // to now() but doesn't preemptively claim tomorrow.
    expect(activeTracksOn([w], tomorrow, PINNED_NOW)).toEqual([]);
  });

  it('drops a track from days after its terminalAt', () => {
    const w = makeTrack({
      createdAt: PINNED_NOW - 4 * DAY_MS,
      terminalAt: PINNED_NOW - 2 * DAY_MS,
    });
    const dayBeforeEnd = new Date(PINNED_NOW - 3 * DAY_MS);
    const endDay = new Date(PINNED_NOW - 2 * DAY_MS);
    const dayAfterEnd = new Date(PINNED_NOW - 1 * DAY_MS);

    expect(activeTracksOn([w], dayBeforeEnd, PINNED_NOW).map((x) => x.id)).toEqual(['w1']);
    expect(activeTracksOn([w], endDay, PINNED_NOW).map((x) => x.id)).toEqual(['w1']);
    expect(activeTracksOn([w], dayAfterEnd, PINNED_NOW)).toEqual([]);
  });

  it('omits a track whose createdAt is after the day in question', () => {
    const w = makeTrack({
      createdAt: PINNED_NOW,
      terminalAt: null,
    });
    const yesterday = new Date(PINNED_NOW - DAY_MS);
    expect(activeTracksOn([w], yesterday, PINNED_NOW)).toEqual([]);
  });

  it('sorts results by createdAt ascending with id tiebreak', () => {
    const wEarly = makeTrack({ id: 'a', createdAt: PINNED_NOW - 4 * DAY_MS });
    const wMid = makeTrack({ id: 'b', createdAt: PINNED_NOW - 2 * DAY_MS });
    const wLate = makeTrack({ id: 'c', createdAt: PINNED_NOW - 2 * DAY_MS });
    // Pass intentionally out-of-order to exercise the sort. All three
    // are open and overlap `today`.
    const today = new Date(PINNED_NOW);
    const out = activeTracksOn([wLate, wEarly, wMid], today, PINNED_NOW);
    expect(out.map((x) => x.id)).toEqual(['a', 'b', 'c']);
    // Atlas area implicit through default areaId.
    expect(out[0].areaId).toBe(atlas.id);
  });
});

describe('TodayPage CalendarCard — track activity dots & agenda', () => {
  function renderTodayWith({
    tracks,
    areas,
    onGo = () => {},
  }: {
    tracks: Track[];
    areas: Area[];
    onGo?: Parameters<typeof TodayPage>[0]['onGo'];
  }) {
    return render(
      <TodayPage
        tracks={tracks}
        areas={areas}
        onGo={onGo}
        todayTerminalId={null}
        todayError={null}
        nowMs={PINNED_NOW}
      />,
    );
  }

  it('paints an area-coloured dot on each day a track is active', () => {
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    const onGo = vi.fn();
    // Track open since 2 days ago — should show dots on the day-before-
    // yesterday, yesterday, and today cells (3 cells in this week).
    const w = makeTrack({
      id: 'w-open',
      areaId: atlas.id,
      createdAt: PINNED_NOW - 2 * DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ tracks: [w], areas: [atlas], onGo });

    // Each .cal-week-day button hosts its own .cal-week-dots span;
    // count how many cells carry a dot. Three days should match (day-
    // before-yesterday, yesterday, today inside the current week).
    const dotCells = document.querySelectorAll('.cal-week-day .cal-week-dot');
    expect(dotCells.length).toBe(3);
    // Every dot carries the area colour (inline style). `#5a9` (CSS
    // shorthand) expands to `#55aa99` → rgb(85, 170, 153); jsdom
    // normalises the inline value through that expansion.
    dotCells.forEach((dot) => {
      expect((dot as HTMLElement).style.background).toBe('rgb(85, 170, 153)');
    });
  });

  it('selecting a day surfaces that day\'s active tracks in the agenda', async () => {
    const user = userEvent.setup();
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    const onGo = vi.fn();
    const w = makeTrack({
      id: 'w-target',
      title: 'Migrate auth',
      areaId: atlas.id,
      lifecycle: 'working',
      createdAt: PINNED_NOW - 1 * DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ tracks: [w], areas: [atlas], onGo });

    // Today's cell defaults selected → agenda already lists the track.
    expect(screen.getByText('Migrate auth')).toBeTruthy();
    // The compact single-line row no longer prints the area name as
    // text (the left coloured bar carries the area identity); instead
    // the area name is folded into the button's aria-label so axe /
    // screen readers still see it.
    const chip = screen.getByRole('button', { name: /Migrate auth/i });
    expect(chip.getAttribute('aria-label')).toContain('in area Atlas');

    // Clicking the chip should dispatch a navigation event for the
    // track id.
    await user.click(chip);
    expect(onGo).toHaveBeenCalledWith({ name: 'track', id: 'w-target' });
  });

  it('uses the shared fallback label for active tracks with empty titles', () => {
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    const w = makeTrack({
      id: 'w-untitled',
      title: '',
      areaId: atlas.id,
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ tracks: [w], areas: [atlas] });

    const chip = screen.getByRole('button', { name: /Untitled track/i });
    expect(chip).toHaveTextContent('Untitled track');
    expect(chip.getAttribute('aria-label')).toContain('Track Untitled track');
  });

  it('renders waiting / running state as a dot flag and folds it into aria-label', () => {
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    const waiting = makeTrack({
      id: 'w-waiting',
      title: 'Needs your input',
      areaId: atlas.id,
      anyCardNeedsInput: true,
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    const running = makeTrack({
      id: 'w-running',
      title: 'Running build',
      areaId: atlas.id,
      lifecycle: 'working',
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ tracks: [waiting, running], areas: [atlas] });

    // Old `cal-event-meta` row is gone — the lifecycle now renders
    // through `.cal-event-lifecycle` on track rows (see test below) and
    // is folded into aria-label for assistive tech.
    expect(document.querySelector('.cal-event-meta')).toBeNull();

    const waitingChip = screen.getByRole('button', { name: /Needs your input/i });
    expect(waitingChip.getAttribute('aria-label')).toContain('waiting on you');
    expect(waitingChip.querySelector('.cal-event-flag.warn')).toBeTruthy();

    const runningChip = screen.getByRole('button', { name: /Running build/i });
    expect(runningChip.getAttribute('aria-label')).toContain('running');
    expect(runningChip.querySelector('.cal-event-flag.run')).toBeTruthy();
  });

  it('track rows surface the lifecycle phrase below the title (and apply `cal-event--track` modifier)', () => {
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    // Cover one quiet, one attention-grabbing, and one running lifecycle
    // so we lock in both the text mapping and the `is-attention` class.
    const reviewing = makeTrack({
      id: 'w-reviewing',
      title: 'Tighten review loop',
      areaId: atlas.id,
      lifecycle: 'reviewing',
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    const working = makeTrack({
      id: 'w-working',
      title: 'Plumb new API',
      areaId: atlas.id,
      lifecycle: 'working',
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    const draft = makeTrack({
      id: 'w-draft',
      title: 'Sketch follow-up',
      areaId: atlas.id,
      lifecycle: 'draft',
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ tracks: [reviewing, working, draft], areas: [atlas] });

    // Every track row carries the `--track` modifier (no hour gutter); the
    // hour-time gutter element is omitted for track rows.
    const rows = document.querySelectorAll('.cal-event');
    expect(rows.length).toBe(3);
    rows.forEach((r) => {
      expect(r.className).toContain('cal-event--track');
      // `.cal-event-time` is the hour gutter — track variant omits it.
      expect(r.querySelector('.cal-event-time')).toBeNull();
      // `.cal-event-lifecycle` lives inside the body below the title.
      expect(r.querySelector('.cal-event-lifecycle')).toBeTruthy();
    });

    // Lifecycle phrase comes from the canonical `lifecycleLabel` helper:
    // `reviewing` → "In review", `working` → "Working", `draft` → "Draft".
    const reviewingRow = screen.getByRole('button', { name: /Tighten review loop/i });
    expect(reviewingRow.querySelector('.cal-event-lifecycle')?.textContent).toBe('In review');
    // `reviewing` is in `isWaitingForUser` bucket → attention modifier.
    expect(
      reviewingRow.querySelector('.cal-event-lifecycle.is-attention'),
    ).toBeTruthy();

    const workingRow = screen.getByRole('button', { name: /Plumb new API/i });
    expect(workingRow.querySelector('.cal-event-lifecycle')?.textContent).toBe('Working');
    // `working` is running, not waiting → no attention modifier.
    expect(
      workingRow.querySelector('.cal-event-lifecycle.is-attention'),
    ).toBeNull();

    const draftRow = screen.getByRole('button', { name: /Sketch follow-up/i });
    expect(draftRow.querySelector('.cal-event-lifecycle')?.textContent).toBe('Draft');
    expect(
      draftRow.querySelector('.cal-event-lifecycle.is-attention'),
    ).toBeNull();
    // The lifecycle phrase is also folded into aria-label so assistive
    // tech sees it regardless of whether CSS loaded.
    expect(draftRow.getAttribute('aria-label')).toContain('Draft');
  });

  it('renders all overlapping tracks into the agenda (CSS clamps height to a scrollable max)', () => {
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    // 20 distinct tracks, all active today — far more than would fit
    // inside the 360px max-height the rail enforces in CSS.
    const tracks: Track[] = Array.from({ length: 20 }, (_, i) =>
      makeTrack({
        id: `w-${i}`,
        title: `Track number ${i}`,
        areaId: atlas.id,
        createdAt: PINNED_NOW - DAY_MS,
        terminalAt: null,
      }),
    );
    renderTodayWith({ tracks, areas: [atlas] });

    // All 20 chips should render into the agenda (no virtualisation):
    // overflow is delegated to CSS (`max-height` + `overflow-y: auto`
    // on `.cal-agenda`). jsdom doesn't load `calm.css`, so we assert
    // the structural invariant — every chip exists in DOM under the
    // agenda container — and leave the visual scroll behaviour to the
    // CSS rule (visible in the production build).
    const agenda = document.querySelector('.cal-agenda') as HTMLElement;
    expect(agenda).toBeTruthy();
    const chips = agenda.querySelectorAll('.cal-event');
    expect(chips.length).toBe(20);
  });

  it('renders long track titles without forcing a multi-line layout (ellipsis class)', () => {
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    const longTitle = 'A'.repeat(200);
    const w = makeTrack({
      id: 'w-long',
      title: longTitle,
      areaId: atlas.id,
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ tracks: [w], areas: [atlas] });

    // The `.cal-event-title` element carries the long text exactly
    // once; the CSS rule (`white-space: nowrap; text-overflow: ellipsis;`
    // in calm.css §cal-event) is what makes it visually truncate. We
    // assert the structural contract — single title element, class
    // applied, text intact — without poking computed styles (calm.css
    // isn't loaded into jsdom; the visual contract is owned by the
    // built bundle and verified in the e2e suite).
    const titleEl = document.querySelector('.cal-event-title') as HTMLElement;
    expect(titleEl).toBeTruthy();
    expect(titleEl.textContent).toBe(longTitle);
    expect(titleEl.className).toContain('cal-event-title');
    // No `.cal-event-meta` survives the redesign — the row is single line.
    expect(document.querySelector('.cal-event-meta')).toBeNull();
  });

  it('shows the empty state on a day with no active tracks and no events', async () => {
    const user = userEvent.setup();
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    // Track that terminated 5 days ago — today's cell has no active
    // track at all.
    const w = makeTrack({
      id: 'w-old',
      areaId: atlas.id,
      createdAt: PINNED_NOW - 10 * DAY_MS,
      terminalAt: PINNED_NOW - 5 * DAY_MS,
    });
    renderTodayWith({ tracks: [w], areas: [atlas] });

    // The default "Today" cell is the one selected at mount. With no
    // active tracks on today, the empty state should render.
    expect(screen.getByText('Nothing scheduled.')).toBeTruthy();
    // Picking yesterday (still inside the track's window) flips the
    // agenda over to the track.
    const dayCells = document.querySelectorAll('.cal-week-day');
    // 7 day buttons; pick a cell with at least one dot.
    const cellWithDot = Array.from(dayCells).find(
      (c) => c.querySelector('.cal-week-dot'),
    );
    expect(cellWithDot).toBeTruthy();
    await user.click(cellWithDot as HTMLElement);
    expect(screen.queryByText('Nothing scheduled.')).toBeNull();
  });

  it('caps week dots at 4 and month dots at 3 per cell', () => {
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    // 6 distinct tracks open today → week cap (4) and month cap (3)
    // should apply.
    const tracks: Track[] = Array.from({ length: 6 }, (_, i) =>
      makeTrack({
        id: `w-${i}`,
        areaId: atlas.id,
        createdAt: PINNED_NOW - DAY_MS,
        terminalAt: null,
      }),
    );
    renderTodayWith({ tracks, areas: [atlas] });

    // Find today's cell (the one with the .today class).
    const todayCell = document.querySelector('.cal-week-day.today');
    expect(todayCell).toBeTruthy();
    const weekDots = (todayCell as HTMLElement).querySelectorAll('.cal-week-dot');
    expect(weekDots.length).toBe(4);
  });
});

describe('TodayPage CalendarCard — month view', () => {
  it('caps month dots at 3 per active cell', async () => {
    const user = userEvent.setup();
    const atlas = makeArea({ id: 'area-atlas', name: 'Atlas', color: '#5a9' });
    const tracks: Track[] = Array.from({ length: 5 }, (_, i) =>
      makeTrack({
        id: `m-${i}`,
        areaId: atlas.id,
        createdAt: PINNED_NOW - DAY_MS,
        terminalAt: null,
      }),
    );
    render(
      <TodayPage
        tracks={tracks}
        areas={[atlas]}
        onGo={() => {}}
        todayTerminalId={null}
        todayError={null}
        nowMs={PINNED_NOW}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Month' }));

    const todayCell = document.querySelector('.cal-month-day.today');
    expect(todayCell).toBeTruthy();
    const monthDots = within(todayCell as HTMLElement).getAllByRole('generic').filter(
      (el) => el.tagName === 'I',
    );
    // Some renderers don't expose <i> as role=generic; fall back to a
    // direct DOM query — same surface either way.
    const directDots = (todayCell as HTMLElement).querySelectorAll('.cal-md-dots i');
    expect(Math.max(monthDots.length, directDots.length)).toBe(3);
  });
});
