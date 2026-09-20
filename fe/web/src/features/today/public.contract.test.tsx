// @vitest-environment jsdom
// Invariants for the Today surface. Behavior lives in public.test.tsx.
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { TodayPage, type ScheduledEvent } from './public.tsx';

import type { TodayPageProps } from './public.tsx';

// A stand-in, not the real TrackRow: `features/today` may not import a sibling domain.
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
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW} />);
    expect(screen.getByRole('complementary').textContent).toContain('Open track');
    expect(screen.queryByText('Nothing scheduled.')).toBeNull();
  });

  it('keeps both sources in the same agenda instead of letting either take over', () => {
    const scheduled = track({ id: 'w2', title: 'Scheduled track', createdAt: NOW - 10 * 86_400_000, terminalAt: NOW - 9 * 86_400_000 });
    const events: ScheduledEvent[] = [{ track: scheduled, date: new Date(NOW), hour: 15 }];
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} scheduledEvents={events} nowMs={NOW} />);

    const agenda = screen.getByRole('complementary').textContent ?? '';
    expect(agenda).toContain('Scheduled track');
    expect(agenda).toContain('Open track');
  });

  it('shows the empty state only when both sources are empty', () => {
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[]} areas={[area()]} nowMs={NOW} />);
    expect(screen.getByText('Nothing scheduled.')).toBeTruthy();
  });

  it('counts a track once when both sources carry it', () => {
    const shared = track({ id: 'w1' });
    const events: ScheduledEvent[] = [{ track: shared, date: new Date(NOW), hour: 9 }];
    render(<TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[shared]} areas={[area()]} scheduledEvents={events} nowMs={NOW} />);
    const today = screen.getByRole('button', { name: 'Monday, Aug 10, 1 track' });
    expect(today.querySelector('[data-nc-day-count]')?.textContent).toBe('1');
  });
});

describe('INV-A11Y-061 navigation shape', () => {
  it('emits no native link anywhere on the surface', () => {
    const { container } = render(
      <TodayPage activityAvailable renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW} />,
    );
    expect(container.querySelectorAll('a').length).toBe(0);
  });
});

/* The stand-in document is a marker, not a report: what is under test is which branch runs. */
const DOCUMENT = <p>the day&apos;s report</p>;
const GUIDE_LABEL = 'Getting started';

describe('INV-TODAYDOC-003 the empty-state predicate is the server field', () => {
  it('renders the empty state for a report nobody has written', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByRole('region', { name: GUIDE_LABEL })).toBeTruthy();
    expect(screen.queryByText("the day's report")).toBeNull();
  });

  it('renders the document once the server says the report has content', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByText("the day's report")).toBeTruthy();
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });

  it('treats a 404 as the empty state rather than an error', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
      launchpad={null} launchpadDocument={DOCUMENT}
    />);
    expect(screen.getByRole('region', { name: GUIDE_LABEL })).toBeTruthy();
  });

  it('says nothing at all while the resolve is still in flight', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
    expect(screen.queryByText("the day's report")).toBeNull();
  });

  it('offers no button anywhere in the main column', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[track({ lifecycle: 'blocked' })]} areas={[area()]} nowMs={NOW}
      launchpad={null}
    />);
    const panel = screen.getByRole('complementary');
    const content = panel.parentElement;
    expect(content).not.toBeNull();
    expect(content?.children.length).toBe(2);
    const mainColumn = [...(content?.children ?? [])].find((child) => child !== panel);
    expect(mainColumn).toBeDefined();
    expect(mainColumn?.contains(screen.getByRole('region', { name: GUIDE_LABEL }))).toBe(true);
    expect(mainColumn?.querySelectorAll('button').length).toBe(0);
  });
});

describe('INV-TODAYDOC-002 a failed resolve never degrades into the empty state', () => {
  it('shows the failure and suppresses the empty state', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[track()]} areas={[area()]} nowMs={NOW}
      launchpad={undefined}
      launchpadDocument={DOCUMENT}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
    />);
    expect(screen.getByRole('alert').textContent).toContain('boom');
    expect(screen.queryByRole('region', { name: 'Getting started' })).toBeNull();
    expect(screen.queryByRole('region', { name: GUIDE_LABEL })).toBeNull();
  });
});

describe('the main column belongs to the document', () => {
  it('omits the Waiting on you list while retaining its header count', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[track({ lifecycle: 'blocked', attention: 'input' })]} areas={[area()]} nowMs={NOW}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      launchpadDocument={DOCUMENT}
    />);
    expect(screen.queryByText('Waiting on you')).toBeNull();
    expect(screen.getByRole('banner').textContent).toContain('1waiting on you');
    expect(screen.getByText("the day's report")).toBeTruthy();
  });
});

describe('#1253 the first-run page keeps the full Today layout', () => {
  /* `areas` excludes the system area, where the launchpad track lives, so "no tracks and no areas" is an ordinary state. */
  it('keeps the calendar and one specific empty state before a launchpad exists', () => {
    render(<TodayPage
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW}
      launchpad={null}
      conversationList={<p>No conversations yet.</p>}
    />);
    expect(screen.getByRole('heading', { name: 'Calendar' })).toBeTruthy();
    expect(screen.getByRole('region', { name: GUIDE_LABEL })).toBeTruthy();
    expect(screen.queryByText('Nothing here yet.')).toBeNull();
  });

  it('renders the report on a workspace with no user areas', () => {
    render(<TodayPage
      activityAvailable
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
      activityAvailable
      renderTrackRow={renderTrackRow} tracks={[]} areas={[]} nowMs={NOW}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
    />);
    expect(screen.getByRole('alert').textContent).toContain('boom');
    expect(screen.queryByRole('region', { name: 'Getting started' })).toBeNull();
  });
});

describe('#1343 the document’s action slot', () => {
  const props = {
    renderTrackRow, tracks: [track()], areas: [area()], nowMs: NOW,
    launchpadDocument: <p>the day&apos;s report</p>,
  } as const;
  const ACTION = <button type="button">Reset</button>;

  it('shows getting-started guidance without document controls when the report is empty', () => {
    render(<TodayPage
      activityAvailable
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: false }}
      documentAction={ACTION}
    />);
    expect(screen.getByRole('region', { name: GUIDE_LABEL })).toBeTruthy();
    expect(screen.queryByText('Nothing written today yet.')).toBeNull();
    expect(screen.getByText('Area')).toBeTruthy();
    expect(screen.getByText('Track')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Reset' })).toBeNull();
    expect(screen.queryByRole('button', { name: /today’s progress/ })).toBeNull();
  });

  it('renders the composition’s action beside a written report', () => {
    render(<TodayPage
      activityAvailable
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
      documentAction={ACTION}
    />);
    expect(screen.getByRole('button', { name: 'Reset' })).toBeTruthy();
    expect(screen.getByText("the day's report")).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'Getting started' })).toBeNull();
  });

  it('renders nothing when no action was supplied', () => {
    render(<TodayPage
      activityAvailable
      {...props}
      launchpad={{ track_id: 'lp', report_has_noninitial_content: true }}
    />);
    expect(screen.queryByRole('button', { name: 'Reset' })).toBeNull();
    expect(screen.getByText("the day's report")).toBeTruthy();
  });

  it('is absent when the resolve itself failed', () => {
    render(<TodayPage
      activityAvailable
      {...props}
      launchpad={undefined}
      launchpadError={<p role="alert">Today&apos;s progress is unavailable: boom</p>}
      documentAction={ACTION}
    />);
    expect(screen.queryByRole('button', { name: 'Reset' })).toBeNull();
    expect(screen.getByRole('alert').textContent).toContain('boom');
    expect(screen.queryByRole('region', { name: 'Getting started' })).toBeNull();
  });
});
