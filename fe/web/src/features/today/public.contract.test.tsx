// @vitest-environment jsdom
// Invariants for the Today surface. Behavior lives in public.test.tsx.
import { cleanup, render, screen } from '@testing-library/react';
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
  return {
    id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0, ...overrides,
  };
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

  it('offers no button anywhere in the main column', () => {
    /*
     * The empty state is one sentence (#1343, owner call). Nothing else stands
     * in this column while the day has no report — no caption, and since #1343
     * no `Rewrite today’s progress` button or Waiting-on-you controls either.
     * The column is identified as the non-panel child of `.content`, so the
     * assertion covers the whole reading column rather than one known wrapper.
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
    // It is the column: it holds the document region and nothing actionable.
    expect(mainColumn?.contains(screen.getByText(EMPTY_COPY))).toBe(true);
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

describe('the main column belongs to the document', () => {
  it('omits the Waiting on you list while retaining its header count', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[track({ lifecycle: 'blocked' })]} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.queryByText('Waiting on you')).toBeNull();
    expect(screen.getByRole('banner').textContent).toContain('1waiting');
    expect(screen.getByText("the day's report")).toBeTruthy();
  });
});

describe('#1253 the first-run page keeps the full Today layout', () => {
  /*
   * `areas` is the USER-visible list: #175 filters the system area out of
   * `GET /api/areas`, and the launchpad track lives in the system area. So
   * "no tracks and no areas" is a perfectly ordinary state for a workspace
   * whose only content is the day's report. It must use the normal two-column
   * layout: an empty data set is not a reason to remove the calendar.
   */
  it('keeps the calendar and one specific empty state before a launchpad exists', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW}
      launchpad={null}
      conversationList={<p>No conversations yet.</p>}
    />);
    expect(screen.getByRole('heading', { name: 'Calendar' })).toBeTruthy();
    expect(screen.getByText('Nothing written today yet.')).toBeTruthy();
    expect(screen.queryByText('Nothing here yet.')).toBeNull();
  });

  it('renders the report on a workspace with no user areas', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
      conversationList={<p>Launchpad conversations</p>}
      conversationAction={<button type="button">New conversation</button>}
    />);
    expect(screen.getByRole('heading', { name: 'Calendar' })).toBeTruthy();
    expect(screen.queryByText('Nothing here yet.')).toBeNull();
    expect(screen.getByText("the day's report")).toBeTruthy();
    expect(screen.getByText('Launchpad conversations')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'New conversation' })).toBeTruthy();
  });

  it('surfaces a failed resolve on a workspace with no user areas', () => {
    render(<TodayPage
      renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
    />);
    expect(screen.getByRole('alert').textContent).toContain('boom');
  });
});

describe('#1343 the document’s action slot', () => {
  const props = {
    renderTrackRow, tracks: [track()], areas: [area()], nowMs: NOW,
    launchpadDocument: <p>the day&apos;s report</p>,
  } as const;
  const ACTION = <button type="button">Reset</button>;

  /*
   * #1343 — the empty state is ONE sentence and nothing else.
   *
   * A `Write` / `Rewrite today’s progress` button used to stand here. It was
   * removed on owner call: the day’s activity now reaches an agent when a
   * conversation is started on the launchpad, injected server-side, so the
   * button was no longer the only route to anything.
   *
   * The action slot is not offered in this branch either, and that is the same
   * ruling rather than an omission: there is nothing to reset when the report
   * is already canonical.
   */
  it('shows one sentence and no controls when the report is empty', () => {
    render(<TodayPage
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
      documentAction={ACTION}
    />);
    expect(screen.getByText('Nothing written today yet.')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Reset' })).toBeNull();
    // The deleted control, pinned by absence so it cannot drift back in
    // without this suite noticing. A label regex, not an exact string: "Write",
    // "Rewrite" and anything else ending in "today’s progress" are all the same
    // growth back.
    expect(screen.queryByRole('button', { name: /today’s progress/ })).toBeNull();
  });

  /* Beside a written document the slot renders exactly what the composition
     layer put in it — the control is wired there because it is destructive and
     needs a confirmation dialog, which is a sibling domain this one may not
     import. */
  it('renders the composition’s action beside a written report', () => {
    render(<TodayPage
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      documentAction={ACTION}
    />);
    expect(screen.getByRole('button', { name: 'Reset' })).toBeTruthy();
    expect(screen.getByText("the day's report")).toBeTruthy();
  });

  /* No slot, no control — not a disabled one. A disabled button is a promise
     it will work later; an absent one is the honest shape for "this
     composition has no action", which is what `features/**` alone can have. */
  it('renders nothing when no action was supplied', () => {
    render(<TodayPage
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
    />);
    expect(screen.queryByRole('button', { name: 'Reset' })).toBeNull();
    expect(screen.getByText("the day's report")).toBeTruthy();
  });

  /* INV-TODAYDOC-002 — a failed resolve shows the failure and nothing else.
     An action on a document the page could not even read has no referent. */
  it('is absent when the resolve itself failed', () => {
    render(<TodayPage
      {...props}
      launchpad={undefined}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
      documentAction={ACTION}
    />);
    expect(screen.queryByRole('button', { name: 'Reset' })).toBeNull();
    expect(screen.getByRole('alert').textContent).toContain('boom');
  });
});
