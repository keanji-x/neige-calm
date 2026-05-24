// Tests for the Today page's calendar rail — issue #250 PR 5.
//
// What we lock in here:
//
//   1. `activeWavesOn` honours the inclusive [createdAt, terminalAt ?? now]
//      predicate (open waves stay active forever; terminated waves drop
//      off after their terminal day; still-future waves stay invisible).
//   2. CalWeek paints one cove-coloured dot per active wave on each
//      day cell, capped at four.
//   3. CalMonth paints up to three cove-coloured dots per active day.
//   4. Selecting a day surfaces that day's active waves in the agenda
//      list (cove name + title visible, click navigates to the wave).
//   5. A day with zero active waves and zero scheduled events shows the
//      "Nothing scheduled." empty state.
//
// Today's terminal panel is mocked out (we pass `todayTerminalId={null}`
// so the calm "booting" line renders; the lazy XtermView never mounts).
//
// jsdom + fake timers note: `nowMs` is passed explicitly so the
// "still-open wave" branch is deterministic across runs.

import { describe, it, expect, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { TodayPage, activeWavesOn } from './Today';
import type { Cove, Wave } from '../types';

// A wave whose terminal_at is `null` is "still open"; the calendar uses
// `Date.now()` to extend its activity through every day up to today.
// The TodayPage component swallows that via its `nowMs` plumbing — we
// pass an explicit value below so the test doesn't depend on wall
// clock drift.

const PINNED_NOW = Date.UTC(2026, 4, 24, 12, 0, 0); // 2026-05-24 12:00 UTC
const DAY_MS = 24 * 60 * 60 * 1000;

function makeCove(overrides: Partial<Cove> = {}): Cove {
  return {
    id: 'cove-atlas',
    name: 'Atlas',
    subtitle: '',
    color: '#5a9',
    ...overrides,
  };
}

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1',
    coveId: 'cove-atlas',
    title: 'Migrate auth',
    lifecycle: 'working',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    createdAt: PINNED_NOW - 3 * DAY_MS,
    terminalAt: null,
    cards: [],
    ...overrides,
  };
}

describe('activeWavesOn', () => {
  const atlas = makeCove();

  it('includes a still-open wave on every day from createdAt to now (inclusive)', () => {
    const w = makeWave({
      createdAt: PINNED_NOW - 2 * DAY_MS,
      terminalAt: null,
    });
    const todayMinus2 = new Date(PINNED_NOW - 2 * DAY_MS);
    const today = new Date(PINNED_NOW);
    const tomorrow = new Date(PINNED_NOW + DAY_MS);

    expect(activeWavesOn([w], todayMinus2, PINNED_NOW).map((x) => x.id)).toEqual(['w1']);
    expect(activeWavesOn([w], today, PINNED_NOW).map((x) => x.id)).toEqual(['w1']);
    // Future days past `now` are NOT included — an open wave grows up
    // to now() but doesn't preemptively claim tomorrow.
    expect(activeWavesOn([w], tomorrow, PINNED_NOW)).toEqual([]);
  });

  it('drops a wave from days after its terminalAt', () => {
    const w = makeWave({
      createdAt: PINNED_NOW - 4 * DAY_MS,
      terminalAt: PINNED_NOW - 2 * DAY_MS,
    });
    const dayBeforeEnd = new Date(PINNED_NOW - 3 * DAY_MS);
    const endDay = new Date(PINNED_NOW - 2 * DAY_MS);
    const dayAfterEnd = new Date(PINNED_NOW - 1 * DAY_MS);

    expect(activeWavesOn([w], dayBeforeEnd, PINNED_NOW).map((x) => x.id)).toEqual(['w1']);
    expect(activeWavesOn([w], endDay, PINNED_NOW).map((x) => x.id)).toEqual(['w1']);
    expect(activeWavesOn([w], dayAfterEnd, PINNED_NOW)).toEqual([]);
  });

  it('omits a wave whose createdAt is after the day in question', () => {
    const w = makeWave({
      createdAt: PINNED_NOW,
      terminalAt: null,
    });
    const yesterday = new Date(PINNED_NOW - DAY_MS);
    expect(activeWavesOn([w], yesterday, PINNED_NOW)).toEqual([]);
  });

  it('sorts results by createdAt ascending with id tiebreak', () => {
    const wEarly = makeWave({ id: 'a', createdAt: PINNED_NOW - 4 * DAY_MS });
    const wMid = makeWave({ id: 'b', createdAt: PINNED_NOW - 2 * DAY_MS });
    const wLate = makeWave({ id: 'c', createdAt: PINNED_NOW - 2 * DAY_MS });
    // Pass intentionally out-of-order to exercise the sort. All three
    // are open and overlap `today`.
    const today = new Date(PINNED_NOW);
    const out = activeWavesOn([wLate, wEarly, wMid], today, PINNED_NOW);
    expect(out.map((x) => x.id)).toEqual(['a', 'b', 'c']);
    // Atlas cove implicit through default coveId.
    expect(out[0].coveId).toBe(atlas.id);
  });
});

describe('TodayPage CalendarCard — wave activity dots & agenda', () => {
  function renderTodayWith({
    waves,
    coves,
    onGo = () => {},
  }: {
    waves: Wave[];
    coves: Cove[];
    onGo?: Parameters<typeof TodayPage>[0]['onGo'];
  }) {
    return render(
      <TodayPage
        waves={waves}
        coves={coves}
        onGo={onGo}
        todayTerminalId={null}
        todayError={null}
        nowMs={PINNED_NOW}
      />,
    );
  }

  it('paints a cove-coloured dot on each day a wave is active', () => {
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    const onGo = vi.fn();
    // Wave open since 2 days ago — should show dots on the day-before-
    // yesterday, yesterday, and today cells (3 cells in this week).
    const w = makeWave({
      id: 'w-open',
      coveId: atlas.id,
      createdAt: PINNED_NOW - 2 * DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ waves: [w], coves: [atlas], onGo });

    // Each .cal-week-day button hosts its own .cal-week-dots span;
    // count how many cells carry a dot. Three days should match (day-
    // before-yesterday, yesterday, today inside the current week).
    const dotCells = document.querySelectorAll('.cal-week-day .cal-week-dot');
    expect(dotCells.length).toBe(3);
    // Every dot carries the cove colour (inline style). `#5a9` (CSS
    // shorthand) expands to `#55aa99` → rgb(85, 170, 153); jsdom
    // normalises the inline value through that expansion.
    dotCells.forEach((dot) => {
      expect((dot as HTMLElement).style.background).toBe('rgb(85, 170, 153)');
    });
  });

  it('selecting a day surfaces that day\'s active waves in the agenda', async () => {
    const user = userEvent.setup();
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    const onGo = vi.fn();
    const w = makeWave({
      id: 'w-target',
      title: 'Migrate auth',
      coveId: atlas.id,
      lifecycle: 'working',
      createdAt: PINNED_NOW - 1 * DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ waves: [w], coves: [atlas], onGo });

    // Today's cell defaults selected → agenda already lists the wave.
    expect(screen.getByText('Migrate auth')).toBeTruthy();
    // The compact single-line row no longer prints the cove name as
    // text (the left coloured bar carries the cove identity); instead
    // the cove name is folded into the button's aria-label so axe /
    // screen readers still see it.
    const chip = screen.getByRole('button', { name: /Migrate auth/i });
    expect(chip.getAttribute('aria-label')).toContain('in cove Atlas');

    // Clicking the chip should dispatch a navigation event for the
    // wave id.
    await user.click(chip);
    expect(onGo).toHaveBeenCalledWith({ name: 'wave', id: 'w-target' });
  });

  it('renders waiting / running state as a dot flag and folds it into aria-label', () => {
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    const waiting = makeWave({
      id: 'w-waiting',
      title: 'Needs your input',
      coveId: atlas.id,
      anyCardNeedsInput: true,
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    const running = makeWave({
      id: 'w-running',
      title: 'Running build',
      coveId: atlas.id,
      lifecycle: 'working',
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ waves: [waiting, running], coves: [atlas] });

    // Old `cal-event-meta` row is gone — the lifecycle now renders
    // through `.cal-event-lifecycle` on wave rows (see test below) and
    // is folded into aria-label for assistive tech.
    expect(document.querySelector('.cal-event-meta')).toBeNull();

    const waitingChip = screen.getByRole('button', { name: /Needs your input/i });
    expect(waitingChip.getAttribute('aria-label')).toContain('waiting on you');
    expect(waitingChip.querySelector('.cal-event-flag.warn')).toBeTruthy();

    const runningChip = screen.getByRole('button', { name: /Running build/i });
    expect(runningChip.getAttribute('aria-label')).toContain('running');
    expect(runningChip.querySelector('.cal-event-flag.run')).toBeTruthy();
  });

  it('wave rows surface the lifecycle phrase below the title (and apply `cal-event--wave` modifier)', () => {
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    // Cover one quiet, one attention-grabbing, and one running lifecycle
    // so we lock in both the text mapping and the `is-attention` class.
    const reviewing = makeWave({
      id: 'w-reviewing',
      title: 'Tighten review loop',
      coveId: atlas.id,
      lifecycle: 'reviewing',
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    const working = makeWave({
      id: 'w-working',
      title: 'Plumb new API',
      coveId: atlas.id,
      lifecycle: 'working',
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    const draft = makeWave({
      id: 'w-draft',
      title: 'Sketch follow-up',
      coveId: atlas.id,
      lifecycle: 'draft',
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ waves: [reviewing, working, draft], coves: [atlas] });

    // Every wave row carries the `--wave` modifier (no hour gutter); the
    // hour-time gutter element is omitted for wave rows.
    const rows = document.querySelectorAll('.cal-event');
    expect(rows.length).toBe(3);
    rows.forEach((r) => {
      expect(r.className).toContain('cal-event--wave');
      // `.cal-event-time` is the hour gutter — wave variant omits it.
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

  it('renders all overlapping waves into the agenda (CSS clamps height to a scrollable max)', () => {
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    // 20 distinct waves, all active today — far more than would fit
    // inside the 360px max-height the rail enforces in CSS.
    const waves: Wave[] = Array.from({ length: 20 }, (_, i) =>
      makeWave({
        id: `w-${i}`,
        title: `Wave number ${i}`,
        coveId: atlas.id,
        createdAt: PINNED_NOW - DAY_MS,
        terminalAt: null,
      }),
    );
    renderTodayWith({ waves, coves: [atlas] });

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

  it('renders long wave titles without forcing a multi-line layout (ellipsis class)', () => {
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    const longTitle = 'A'.repeat(200);
    const w = makeWave({
      id: 'w-long',
      title: longTitle,
      coveId: atlas.id,
      createdAt: PINNED_NOW - DAY_MS,
      terminalAt: null,
    });
    renderTodayWith({ waves: [w], coves: [atlas] });

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

  it('shows the empty state on a day with no active waves and no events', async () => {
    const user = userEvent.setup();
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    // Wave that terminated 5 days ago — today's cell has no active
    // wave at all.
    const w = makeWave({
      id: 'w-old',
      coveId: atlas.id,
      createdAt: PINNED_NOW - 10 * DAY_MS,
      terminalAt: PINNED_NOW - 5 * DAY_MS,
    });
    renderTodayWith({ waves: [w], coves: [atlas] });

    // The default "Today" cell is the one selected at mount. With no
    // active waves on today, the empty state should render.
    expect(screen.getByText('Nothing scheduled.')).toBeTruthy();
    // Picking yesterday (still inside the wave's window) flips the
    // agenda over to the wave.
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
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    // 6 distinct waves open today → week cap (4) and month cap (3)
    // should apply.
    const waves: Wave[] = Array.from({ length: 6 }, (_, i) =>
      makeWave({
        id: `w-${i}`,
        coveId: atlas.id,
        createdAt: PINNED_NOW - DAY_MS,
        terminalAt: null,
      }),
    );
    renderTodayWith({ waves, coves: [atlas] });

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
    const atlas = makeCove({ id: 'cove-atlas', name: 'Atlas', color: '#5a9' });
    const waves: Wave[] = Array.from({ length: 5 }, (_, i) =>
      makeWave({
        id: `m-${i}`,
        coveId: atlas.id,
        createdAt: PINNED_NOW - DAY_MS,
        terminalAt: null,
      }),
    );
    render(
      <TodayPage
        waves={waves}
        coves={[atlas]}
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
