// @vitest-environment jsdom
// Invariants for the Today surface. Behavior lives in public.test.tsx.
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
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
    const today = screen.getByRole('button', { name: 'Monday, Aug 10, 1 wave' });
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

/*
 * #1253 §5.2 — the Today document region.
 *
 * `launchpad` is the server's answer to `GET /api/today/launchpad`, and
 * `report_has_noninitial_content` is the ONLY thing on this page that decides
 * between the document and the empty state. The stand-in document below is a
 * marker, not a report: what is under test is which branch runs, and the real
 * `ReportDocument` against a real canonical initial payload is exercised at the
 * composition layer in `app/router/today-document.test.tsx`.
 */
const DOCUMENT = <p>the day&apos;s report</p>;
const EMPTY_COPY = 'Nothing written today yet.';

describe('INV-TODAYDOC-003 the empty-state predicate is the server field', () => {
  it('renders the empty state for a report nobody has written', () => {
    render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[wave()]} coves={[cove()]} nowMs={NOW}
      launchpad={{ wave_id: 'lp', report_has_noninitial_content: false }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByText(EMPTY_COPY)).toBeTruthy();
    // The negative half: the canonical initial report is a well-formed
    // document — four empty H1s — so a page that decided this by looking at
    // the document instead of at the server field would render those headings
    // here rather than the empty state.
    expect(screen.queryByText("the day's report")).toBeNull();
  });

  it('renders the document once the server says the report has content', () => {
    render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[wave()]} coves={[cove()]} nowMs={NOW}
      launchpad={{ wave_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByText("the day's report")).toBeTruthy();
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });

  it('treats a 404 as the empty state rather than an error', () => {
    render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[wave()]} coves={[cove()]} nowMs={NOW}
      launchpad={null} launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByText(EMPTY_COPY)).toBeTruthy();
  });

  it('says nothing at all while the resolve is still in flight', () => {
    // "We do not know yet" and "there is nothing" are different answers, and
    // flashing the second one while the first is true is how a page teaches
    // people to distrust it.
    render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[wave()]} coves={[cove()]} nowMs={NOW}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
    expect(screen.queryByText("the day's report")).toBeNull();
  });

  it('offers no trigger button anywhere in the main column', () => {
    /*
     * `POST /api/today/summary` does not exist until PR2. A stubbed, mocked or
     * disabled button would be worse than its absence.
     *
     * Asserted as "no button at all in the main column", not as "no button
     * inside the empty-state paragraph" (that paragraph is a `<p>` with one
     * text node, so querying it for a button is null whatever production does)
     * and not as a label regex either — "Generate", "Run" or a Chinese label
     * would walk straight past one. The workspace is seeded with no waves so
     * the column holds only the document region and the terminal placeholder,
     * neither of which has a control today.
     */
    const { container } = render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[]} coves={[cove()]} nowMs={NOW}
      launchpad={null}
    />);
    const empty = screen.getByText(EMPTY_COPY);
    const mainColumn = empty.parentElement;
    expect(mainColumn).not.toBeNull();
    expect(mainColumn?.querySelectorAll('button').length).toBe(0);
    // And the region really is the main column, not some stray wrapper: the
    // terminal placeholder is its sibling.
    expect(within(container).getByText('Terminal is not wired up yet.').closest('section')?.parentElement)
      .toBe(mainColumn);
  });
});

describe('INV-TODAYDOC-002 a failed resolve never degrades into the empty state', () => {
  it('shows the failure and suppresses the empty state', () => {
    render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[wave()]} coves={[cove()]} nowMs={NOW}
      launchpad={undefined}
      launchpadDocument={DOCUMENT}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
    />);
    expect(screen.getByRole('alert').textContent).toContain('boom');
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });
});

describe('#1253 D7 the status bar comes before the document', () => {
  it('puts the waiting rows above the document in the main column', () => {
    const { container } = render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[wave({ lifecycle: 'blocked' })]} coves={[cove()]} nowMs={NOW}
      launchpad={{ wave_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    const main = within(container).getByText('Waiting on you');
    const document_ = within(container).getByText("the day's report");
    expect(main.compareDocumentPosition(document_) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });
});

describe('#1253 D7 the status bar is O(1) in height', () => {
  /*
   * D7 puts the status bar above the document *because* its height does not
   * depend on the workspace, and that is the whole justification for the
   * order. `waiting` has no natural bound, so without a cap the justification
   * is false — a review found it false at 100 blocked waves, with the report
   * pushed off the first screen.
   */
  const manyWaiting = Array.from({ length: 100 }, (_, index) => wave({
    id: `blocked-${index}`, title: `Blocked ${index}`, lifecycle: 'blocked',
  }));

  it('caps the waiting rows so the document cannot be pushed down', () => {
    const { container } = render(<TodayPage
      renderWaveRow={renderWaveRow} waves={manyWaiting} coves={[cove()]} nowMs={NOW}
      launchpad={{ wave_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    const waitingLabel = within(container).getByText('Waiting on you');
    const section = waitingLabel.closest('section');
    expect(section?.querySelectorAll('[data-nc-role="row"]').length).toBe(5);
    // The count that is not shown is stated rather than dropped.
    expect(screen.getByRole('button', { name: '+95 more waiting' })).toBeTruthy();
    // The header still reports the true total, so the cap hides no fact.
    expect(screen.getByRole('banner').textContent).toContain('100waiting');
  });

  it('keeps every waiting wave reachable behind the control', async () => {
    const { container } = render(<TodayPage
      renderWaveRow={renderWaveRow} waves={manyWaiting} coves={[cove()]} nowMs={NOW}
      launchpad={{ wave_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    /* Scoped to the status bar, because a waiting wave whose lifespan overlaps
       the selected day also shows on the calendar agenda — so an unscoped
       `queryByText` would be answered by the panel and prove nothing about the
       cap. RUNNING and RECENT both exclude anything already counted as
       waiting, so this control is the status bar's only route to the rest. */
    const waiting = () => within(container).getByText('Waiting on you').closest('section');
    const rows = () => [...(waiting()?.querySelectorAll('[data-nc-role="row"]') ?? [])]
      .map((row) => row.textContent);
    expect(rows()).not.toContain('Blocked 99');
    await userEvent.click(screen.getByRole('button', { name: '+95 more waiting' }));
    expect(rows()).toContain('Blocked 99');
    expect(rows().length).toBe(100);
    expect(screen.getByRole('button', { name: 'Show fewer' })).toBeTruthy();
  });

  it('draws no control when the waiting list already fits', () => {
    render(<TodayPage
      renderWaveRow={renderWaveRow} waves={manyWaiting.slice(0, 5)} coves={[cove()]} nowMs={NOW}
      launchpad={{ wave_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.queryByRole('button', { name: /more waiting/ })).toBeNull();
  });
});

describe('#1253 the first-run page still owns a document', () => {
  /*
   * `coves` is the USER-visible list: #175 filters the system cove out of
   * `GET /api/coves`, and the launchpad wave lives in the system cove. So
   * "no waves and no coves" is a perfectly ordinary state for a workspace
   * whose only content is the day's report — and the early return for it used
   * to drop the document and the resolve failure alike.
   */
  it('renders the report on a workspace with no user coves', () => {
    render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[]} coves={[]} nowMs={NOW}
      launchpad={{ wave_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByText('Nothing here yet.')).toBeTruthy();
    expect(screen.getByText("the day's report")).toBeTruthy();
  });

  it('surfaces a failed resolve on a workspace with no user coves', () => {
    render(<TodayPage
      renderWaveRow={renderWaveRow} waves={[]} coves={[]} nowMs={NOW}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
    />);
    expect(screen.getByRole('alert').textContent).toContain('boom');
  });
});
