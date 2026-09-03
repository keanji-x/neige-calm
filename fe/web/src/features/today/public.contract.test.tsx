// @vitest-environment jsdom
// Invariants for the Today surface. Behavior lives in public.test.tsx.
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { TodayPage, type ScheduledEvent } from './public.tsx';

import type { TodayPageProps } from './public.tsx';

// A stand-in, not the real TrackRow: `features/today` may not import a sibling
// domain, and these suites are about Today's own bucketing and layout. The real
// row has its own tests, and `app/router` is where the two are composed.
const renderTrackRow: TodayPageProps['renderTrackRow'] = (track, options) => (
  <span data-nc-role="row" data-nc-state={options.variant === 'panel' ? 'selected' : undefined}>
    {options.hourLabel}{track.title}
  </span>
);

afterEach(cleanup);

const NOW = new Date(2026, 7, 10, 15, 0, 0).getTime();

function area(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Open track', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: NOW - 3_600_000, updatedAt: NOW,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

describe('INV-TODAY-002 the scheduled-event seam', () => {
  it('renders live track activity while the scheduled list is empty', () => {
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW} />);
    expect(screen.getByRole('complementary').textContent).toContain('Open track');
    expect(screen.queryByText('Nothing scheduled.')).toBeNull();
  });

  it('keeps both sources in the same agenda instead of letting either take over', () => {
    // A scheduling plugin has not landed, so production always passes an empty
    // list. Feeding a synthetic event through the seam is the only way to prove
    // the branch still exists and still co-exists with track activity — deleting
    // it as "dead code" is exactly the regression this locks.
    const scheduled = track({ id: 'w2', title: 'Scheduled track', createdAt: NOW - 10 * 86_400_000, terminalAt: NOW - 9 * 86_400_000 });
    const events: ScheduledEvent[] = [{ track: scheduled, date: new Date(NOW), hour: 15 }];
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} scheduledEvents={events} nowMs={NOW} />);

    const agenda = screen.getByRole('complementary').textContent ?? '';
    expect(agenda).toContain('Scheduled track');
    expect(agenda).toContain('Open track');
  });

  it('shows the empty state only when both sources are empty', () => {
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[]} areas={[area()]} nowMs={NOW} />);
    expect(screen.getByText('Nothing scheduled.')).toBeTruthy();
  });

  it('counts a track once when both sources carry it', () => {
    // The day cell shows how many tracks a day holds, so double-counting is the
    // failure this locks: one track present in both sources must read as 1.
    const shared = track({ id: 'w1' });
    const events: ScheduledEvent[] = [{ track: shared, date: new Date(NOW), hour: 9 }];
    render(<TodayPage renderTrackRow={renderTrackRow} tracks={[shared]} areas={[area()]} scheduledEvents={events} nowMs={NOW} />);
    // Both the drawn glyph and the accessible name say one, not two.
    const today = screen.getByRole('button', { name: 'Monday, Aug 10, 1 track' });
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
   * `features/track/row/public.test.tsx`.
   */
  it('emits no native link anywhere on the surface', () => {
    const { container } = render(
      <TodayPage renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW} />,
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
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
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
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByText("the day's report")).toBeTruthy();
    expect(screen.queryByText(EMPTY_COPY)).toBeNull();
  });

  it('treats a 404 as the empty state rather than an error', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
      launchpad={null} launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByText(EMPTY_COPY)).toBeTruthy();
  });

  it('says nothing at all while the resolve is still in flight', () => {
    // "We do not know yet" and "there is nothing" are different answers, and
    // flashing the second one while the first is true is how a page teaches
    // people to distrust it.
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
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
     * would walk straight past one.
     *
     * THE COLUMN IS FOUND FROM THE OUTSIDE IN, not by walking up from the
     * empty line, and that is the whole point of the shape below.
     *
     * Walking up N `parentElement` hops cannot state which element it landed
     * on; it can only state how far it climbed, so any wrapper inserted
     * between the column and the empty line moves the landing spot down while
     * the test stays green. Adding "…and it contains the waiting heading" does
     * not fix that:
     * one wrapper around BOTH `WaitingSection` and `TodayDocument` satisfies
     * it, and a button added in the column outside that wrapper then goes
     * unseen. So the walk is replaced by an identification: `.content` has
     * exactly two children, the panel is the one carrying `role=complementary`
     * (`<aside>`), and the main column is the other one. Nothing inside the
     * column can change that, which is what makes "anywhere in the main
     * column" true as written.
     *
     * The workspace is seeded with ONE BLOCKED TRACK so the column has a
     * second region besides the document — the assertions below check the
     * landed element really does hold both, i.e. it is the column and not one
     * of its children. One blocked track and not six: `WAITING_ROW_LIMIT` is
     * 5, and the sixth would add the "+N more waiting" disclosure — a real
     * button in the main column, which this assertion would have to carve out.
     */
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[track({ lifecycle: 'blocked' })]} areas={[area()]} nowMs={NOW}
      launchpad={null}
    />);
    const panel = screen.getByRole('complementary');
    const content = panel.parentElement;
    expect(content).not.toBeNull();
    // Pinned rather than assumed: if the row ever grows a third child, the
    // "the other one" step below stops being well defined and this says so.
    expect(content?.children.length).toBe(2);
    const mainColumn = [...(content?.children ?? [])].find((child) => child !== panel);
    expect(mainColumn).toBeDefined();
    // It is the column: it holds the document region and the status bar, the
    // two things the column is made of.
    expect(mainColumn?.contains(screen.getByText(EMPTY_COPY))).toBe(true);
    expect(mainColumn?.contains(screen.getByText('Waiting on you'))).toBe(true);
    expect(mainColumn?.querySelectorAll('button').length).toBe(0);
  });
});

describe('INV-TODAYDOC-002 a failed resolve never degrades into the empty state', () => {
  it('shows the failure and suppresses the empty state', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
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
      renderTrackRow={renderTrackRow} tracks={[track({ lifecycle: 'blocked' })]} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
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
   * is false — a review found it false at 100 blocked tracks, with the report
   * pushed off the first screen.
   */
  const manyWaiting = Array.from({ length: 100 }, (_, index) => track({
    id: `blocked-${index}`, title: `Blocked ${index}`, lifecycle: 'blocked',
  }));

  it('caps the waiting rows so the document cannot be pushed down', () => {
    const { container } = render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={manyWaiting} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
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

  it('keeps every waiting track reachable behind the control', async () => {
    const { container } = render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={manyWaiting} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    /* Scoped to the status bar, because a waiting track whose lifespan overlaps
       the selected day also shows on the calendar agenda — so an unscoped
       `queryByText` would be answered by the panel and prove nothing about the
       cap. RUNNING excludes anything already counted as waiting, so this
       control is the status bar's only route to the rest. */
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
      renderTrackRow={renderTrackRow} tracks={manyWaiting.slice(0, 5)} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.queryByRole('button', { name: /more waiting/ })).toBeNull();
  });
});

describe('#1253 the first-run page still owns a document', () => {
  /*
   * `areas` is the USER-visible list: #175 filters the system area out of
   * `GET /api/areas`, and the launchpad track lives in the system area. So
   * "no tracks and no areas" is a perfectly ordinary state for a workspace
   * whose only content is the day's report — and the early return for it used
   * to drop the document and the resolve failure alike.
   */
  it('renders the report on a workspace with no user areas', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByText('Nothing here yet.')).toBeTruthy();
    expect(screen.getByText("the day's report")).toBeTruthy();
  });

  it('surfaces a failed resolve on a workspace with no user areas', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
    />);
    expect(screen.getByRole('alert').textContent).toContain('boom');
  });
});

describe('#1253 D5 the document’s trigger', () => {
  const WRITE = 'Write today’s progress';
  const REWRITE = 'Rewrite today’s progress';
  const props = {
    renderTrackRow, tracks: [track()], areas: [area()], nowMs: NOW,
    launchpadDocument: <p>the day&apos;s report</p>,
  } as const;

  /*
   * No control at all when the composition offers none — not a disabled one.
   *
   * A disabled button is a promise it will work later. An absent one is the
   * honest shape for "this composition has no trigger", which is what every
   * suite in this file passes and what `features/**` alone can ever have: the
   * endpoint lives in `app/router`.
   */
  it('renders nothing when no trigger was supplied', () => {
    render(<TodayPage {...props} launchpad={{ track_id: 'lp', report_has_noninitial_content: false }} />);
    expect(screen.queryByRole('button', { name: WRITE })).toBeNull();
    expect(screen.queryByRole('button', { name: REWRITE })).toBeNull();
  });

  /*
   * The control appears whether or not anything happened today.
   *
   * This page cannot know — the design gives it no activity read, deliberately
   * (D4 deleted the layer that would have offered one) — so hiding the button
   * would be a guess, and the wrong guess makes the feature look broken. The
   * gate is `POST /api/today/summary`'s, which refuses an empty window without
   * creating a conversation or sending a message (INV-TODAYDOC-007).
   */
  it('offers the trigger in the empty state and a re-run once the report has content', async () => {
    const pressed: string[] = [];
    const { rerender } = render(<TodayPage
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
      onWriteSummary={() => pressed.push('empty')}
    />);
    await userEvent.click(screen.getByRole('button', { name: WRITE }));
    expect(pressed).toEqual(['empty']);

    rerender(<TodayPage
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      onWriteSummary={() => pressed.push('rerun')}
    />);
    await userEvent.click(screen.getByRole('button', { name: REWRITE }));
    expect(pressed).toEqual(['empty', 'rerun']);
    expect(screen.getByText("the day's report")).toBeTruthy();
  });

  /* In flight the control says so and cannot fire again — one press, one
     request, however fast the user is. */
  it('is inert while a request is in flight', async () => {
    const pressed: string[] = [];
    render(<TodayPage
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
      onWriteSummary={() => pressed.push('again')}
      summaryPending
    />);
    const button = screen.getByRole('button', { name: 'Writing…' });
    expect(button.getAttribute('aria-busy')).toBe('true');
    await userEvent.click(button);
    expect(pressed).toEqual([]);
  });

  /* The notice sits beside the button, not in place of the document: a refused
     or failed trigger changed nothing about the report already on screen. */
  it('shows the trigger’s answer without replacing the report', () => {
    render(<TodayPage
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      onWriteSummary={() => undefined}
      summaryNotice={<span>Nothing has happened in this workspace today yet.</span>}
    />);
    expect(screen.getByText('Nothing has happened in this workspace today yet.')).toBeTruthy();
    expect(screen.getByText("the day's report")).toBeTruthy();
  });

  /* INV-TODAYDOC-002 — a failed resolve shows the failure and nothing else.
     The trigger would rewrite a document the page could not even read. */
  it('is absent when the resolve itself failed', () => {
    render(<TodayPage
      {...props}
      launchpad={undefined}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
      onWriteSummary={() => undefined}
    />);
    expect(screen.queryByRole('button', { name: WRITE })).toBeNull();
    expect(screen.queryByRole('button', { name: REWRITE })).toBeNull();
  });
});
