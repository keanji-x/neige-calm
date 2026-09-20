// Today — the landing route. Presentational and props-driven: the data comes from app/router.

import { Calendar as AstryxCalendar, type ISODateString } from '@astryxdesign/core/Calendar';
import { useEffect, useMemo, useRef, type ReactNode } from 'react';

import {
  activeTracksOn, hasFailed, isRunning, needsUserAttention, visibleTracks, type Track,
} from '../../../../core/domain/track.ts';
import { areaOf, type Area } from '../../../../core/domain/area.ts';
import type { TodayLaunchpadWire } from '../../../../core/domain/today.ts';
import type {
  ScheduledEvent, TodayCompactProps, TodayPageProps, TrackRowRenderer,
} from './page-props.ts';
import { PageHeader, PageTitle } from '../../ui/page-header/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import { PanelCard, PanelEmpty, PanelModule } from '../../ui/panel-card/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { ViewportDispatch } from './viewport-dispatch.tsx';
import styles from './today.module.css';

export type { ScheduledEvent, TodayPageProps, TrackRowRenderer } from './page-props.ts';

const SHORT_DAYS = Object.freeze(['M', 'T', 'W', 'T', 'F', 'S', 'S'] as const);

/** The week grid's section label: both months on a crossing week, both years on a New Year's week. */
function weekLabel(weekStart: Date, weekEnd: Date): string {
  const long = (date: Date, options: Intl.DateTimeFormatOptions) =>
    date.toLocaleDateString('en-US', options);
  if (weekStart.getMonth() === weekEnd.getMonth() && weekStart.getFullYear() === weekEnd.getFullYear()) {
    return long(weekStart, { month: 'long', year: 'numeric' });
  }
  if (weekStart.getFullYear() === weekEnd.getFullYear()) {
    return `${long(weekStart, { month: 'short' })} – ${long(weekEnd, { month: 'short', year: 'numeric' })}`;
  }
  return `${long(weekStart, { month: 'short', year: 'numeric' })} – ${long(weekEnd, { month: 'short', year: 'numeric' })}`;
}

/** An agenda row always names an area; an unresolvable id says so rather than going silent. */
const UNKNOWN_AREA = 'Unknown area';

function addDays(day: Date, count: number): Date {
  const next = new Date(day);
  next.setDate(next.getDate() + count);
  return next;
}

function startOfWeek(day: Date): Date {
  const start = new Date(day);
  start.setDate(start.getDate() - ((start.getDay() + 6) % 7));
  start.setHours(0, 0, 0, 0);
  return start;
}

function sameDay(left: Date, right: Date): boolean {
  return left.getFullYear() === right.getFullYear()
    && left.getMonth() === right.getMonth()
    && left.getDate() === right.getDate();
}

function formatHour(hour: number): string {
  return `${(hour + 11) % 12 + 1}${hour >= 12 ? 'pm' : 'am'}`;
}

function isoDate(day: Date): ISODateString {
  const month = String(day.getMonth() + 1).padStart(2, '0');
  const date = String(day.getDate()).padStart(2, '0');
  return `${day.getFullYear()}-${month}-${date}` as ISODateString;
}

/** The current time and the day it falls in. A pinned `nowMs` freezes it; an unpinned one ticks every 15s. Held by each renderer, so crossing the breakpoint resamples the clock. */
function useNow(nowMs: number | undefined): Readonly<{ now: Date; today: Date }> {
  const [now, setNow] = useState<Date>(() => (nowMs === undefined ? new Date() : new Date(nowMs)));

  useEffect(() => {
    if (nowMs !== undefined) {
      setNow(new Date(nowMs));
      return;
    }
    const id = setInterval(() => setNow(new Date()), 15_000);
    return () => clearInterval(id);
  }, [nowMs]);

  const today = useMemo(() => {
    const start = new Date(now);
    start.setHours(0, 0, 0, 0);
    return start;
  }, [now]);

  return { now, today };
}

/** Today, as two renderers. This function does not know which viewport it is on: `ViewportDispatch` holds the bit, and `compactProps` is typed `TodayCompactProps`, so anything the ledger excludes is an excess property. */
export function TodayPage(props: TodayPageProps) {
  return (
    <ViewportDispatch<TodayCompactProps, TodayPageProps>
      compact={TodayCompact}
      compactProps={{ nowMs: props.nowMs }}
      desktop={TodayDesktop}
      desktopProps={props}
    />
  );
}

/* The two renderers' real parameter types, exported so the ledger's contract test can pin them from outside this file, where the names cannot be shadowed. */
export type TodayPageSignature = Parameters<typeof TodayPage>[0];
export type TodayCompactSignature = Parameters<typeof TodayCompact>[0];

/** The phone: a header and the month calendar. Its props type is the ledger's `render: true` half. */
function TodayCompact({ nowMs }: TodayCompactProps) {
  const { today } = useNow(nowMs);
  return (
    <main className={styles.mobileToday}>
      <MobileHeader title="Today" level={1} />
      <AstryxCalendar
        key={isoDate(today)}
        defaultValue={isoDate(today)}
        weekStartsOn="mon"
        hasVariableRowCount
      />
    </main>
  );
}

function TodayDesktop({
  tracks, areas, renderTrackRow, scheduledEvents = [], conversationList, conversationAction,
  launchpad, launchpadDocument, launchpadError, nowMs,
  documentAction, activityAvailable,
}: TodayPageProps) {
  const { now, today } = useNow(nowMs);

  const shownTracks = visibleTracks(tracks);
  /* Grouped by lifecycle phase, indicated by activity: `waiting` is the kernel's verdict (input or failed); "In progress" is the phase, not `isWorking`. */
  const needsPerson = (track: Track) => needsUserAttention(track) || hasFailed(track);
  const waiting = shownTracks.filter(needsPerson);
  const inProgress = shownTracks.filter((track) => isRunning(track.lifecycle) && !needsPerson(track));
  const panel = (
    <aside className={styles.panelColumn} data-nc-panel="">
      <PanelCard>
        <PanelModule title="Calendar">
          <Calendar
            activityAvailable={activityAvailable}
            today={today}
            tracks={shownTracks}
            areas={areas}
            scheduledEvents={scheduledEvents}
            renderTrackRow={renderTrackRow}
            nowMs={now.getTime()}
          />
        </PanelModule>
        <PanelRows title="In progress" tracks={inProgress} render={renderTrackRow} />
        <PanelModule title="Conversations" action={conversationAction}>{conversationList}</PanelModule>
      </PanelCard>
    </aside>
  );
  return (
    <div className={styles.page}>
      <TodayHeader
        activityAvailable={activityAvailable}
        today={today} waiting={waiting.length} inProgress={inProgress.length}
        now={now}
      />
      <div className={styles.content}>
        <div className={styles.mainColumn}>
          <TodayDocument
            launchpad={launchpad}
            document={launchpadDocument}
            error={launchpadError}
            action={documentAction}
          />
        </div>

        {/* `data-nc-panel` is how `app/shell` hides this while the conversation drawer is open; a CSS Module class is not nameable from the shell's stylesheet. */}
        {panel}
      </div>
    </div>
  );
}

/** The document region: the day's report, or the reason there is none. The branch order is the invariant: an error must not fall through into the empty state, and the empty state is decided by the server's `report_has_noninitial_content` alone. */
function TodayDocument({ launchpad, document, error, action }: {
  launchpad?: TodayLaunchpadWire | null;
  document?: ReactNode;
  error?: ReactNode;
  action?: ReactNode;
}) {
  if (error !== undefined && error !== null) return <>{error}</>;
  // The read is still in flight: not the empty state.
  if (launchpad === undefined) return null;
  const written = launchpad !== null && launchpad.report_has_noninitial_content;
  if (!written) {
    return (
      <div className={`${styles.document} ${styles.documentVacant}`}>
        <section className={styles.documentGuide} aria-label="Getting started">
          <dl className={styles.guideConcepts}>
            <dt>Area</dt>
            <dd>Keep a project’s context and related work together.</dd>
            <dt>Track</dt>
            <dd>Give an agent a goal and keep its result in a report.</dd>
          </dl>
          <p>Use + beside Areas to create one, then + beside its name to start a Track.</p>
          <p>Start a conversation with Today to gather your progress here.</p>
        </section>
      </div>
    );
  }
  return (
    <div className={styles.document}>
      {document}
      {action !== undefined && action !== null && (
        <div className={styles.documentAction}>{action}</div>
      )}
    </div>
  );
}

/** A track list as a panel module, rendered only when it has rows. The `panel` variant is the agenda's: `app/router` keys the row's delete affordance off it. */
function PanelRows({ title, tracks, render }: {
  title: string;
  tracks: readonly Track[];
  render: TrackRowRenderer;
}) {
  if (tracks.length === 0) return null;
  return (
    <PanelModule title={title}>
      <div className={styles.rows}>
        {tracks.map((track) => (
          <span key={track.id}>{render(track, { variant: 'compact' })}</span>
        ))}
      </div>
    </PanelModule>
  );
}

function TodayHeader({ today, waiting, inProgress, now, activityAvailable }: {
  today: Date;
  waiting: number;
  inProgress: number;
  now: Date;
  activityAvailable: boolean;
}) {
  return (
    <PageHeader
      title={
        <PageTitle>
          {today.toLocaleDateString('en-US', { weekday: 'long', month: 'long', day: 'numeric' })}
        </PageTitle>
      }
      meta={activityAvailable ? (
        <span className={styles.counts}>
          <span className={styles.countValue}>{waiting}</span>
          <span className={styles.countWord}>waiting on you</span>
          <span className={styles.countSep} aria-hidden="true">·</span>
          <span className={styles.countValue}>{inProgress}</span>
          <span className={styles.countWord}>in progress</span>
        </span>
      ) : undefined}
      actions={<Clock now={now} />}
    />
  );
}

/** Ambient, so position is its entire signal. No seconds. */
function Clock({ now }: { now: Date }) {
  const hours = now.getHours();
  return (
    <span className={styles.clock}>
      {`${(hours + 11) % 12 + 1}:${String(now.getMinutes()).padStart(2, '0')} ${hours >= 12 ? 'PM' : 'AM'}`}
    </span>
  );
}

function Calendar({ today, tracks, areas, scheduledEvents, renderTrackRow, nowMs, activityAvailable }: {
  today: Date;
  tracks: readonly Track[];
  areas: readonly Area[];
  scheduledEvents: readonly ScheduledEvent[];
  renderTrackRow: TrackRowRenderer;
  nowMs?: number;
  activityAvailable: boolean;
}) {
  const [selected, setSelected] = useState<Date>(today);
  const previousToday = useRef(today);
  useEffect(() => {
    setSelected((current) => sameDay(current, previousToday.current) ? today : current);
    previousToday.current = today;
  }, [today]);
  const now = nowMs ?? Date.now();
  const weekStart = startOfWeek(selected);
  const days = Array.from({ length: 7 }, (_, index) => addDays(weekStart, index));

  const scheduledAgenda = scheduledEvents
    .filter((event) => sameDay(event.date, selected))
    .toSorted((left, right) => left.hour - right.hour);
  const trackAgenda = activeTracksOn(tracks, selected, now);
  const scheduledIds = new Set(scheduledAgenda.map((event) => event.track.id));

  return (
    <div className={styles.calendar}>
      <div className={styles.week}>
        <div className={styles.weekHead}>
          <button type="button" data-nc-role="icon" className={styles.navButton}
            aria-label="Previous week" onClick={() => setSelected(addDays(selected, -7))}><Icon name="chevron-left" /></button>
          <span className={styles.monthLabel}>
            {weekLabel(weekStart, addDays(weekStart, 6))}
          </span>
          <button type="button" data-nc-role="icon" className={styles.navButton}
            aria-label="Next week" onClick={() => setSelected(addDays(selected, 7))}><Icon name="chevron-right" /></button>
        </div>

        <div className={styles.dayNames} aria-hidden="true">
          {SHORT_DAYS.map((day, index) => (
            <span key={index} className={styles.dayName}>{day}</span>
          ))}
        </div>

        <div className={styles.weekGrid}>
          {days.map((day) => {
            // De-dup by track id: a track with both a scheduled event and an
            // overlapping activity window is counted once, not twice.
            const seen = new Set<string>();
            for (const event of scheduledEvents.filter((candidate) => sameDay(candidate.date, day))) {
              seen.add(event.track.id);
            }
            for (const track of activeTracksOn(tracks, day, now)) {
              seen.add(track.id);
            }
            const isToday = sameDay(day, today);
            const isSelected = sameDay(day, selected);
            return (
              <button
                key={day.toDateString()}
                type="button"
                data-nc-role="cell"
                className={[
                  styles.day, isToday ? styles.dayToday : '', isSelected ? styles.daySelected : '',
                ].filter(Boolean).join(' ')}
                aria-pressed={isSelected}
                /* The count belongs in the accessible name: the superscript mark is hidden from assistive tech. */
                aria-label={day.toLocaleDateString('en-US', { weekday: 'long', month: 'short', day: 'numeric' })
                  + (!activityAvailable || seen.size === 0 ? '' : `, ${seen.size} track${seen.size === 1 ? '' : 's'}`)}
                onClick={() => setSelected(day)}
              >
                <span className={styles.dayNumber}>{day.getDate()}</span>
                {activityAvailable && seen.size > 0 && (
                  <span className={styles.dayCount} data-nc-day-count={seen.size} aria-hidden="true">
                    {seen.size}
                  </span>
                )}
              </button>
            );
          })}
        </div>
      </div>

      <div className={styles.agenda}>
          {!sameDay(selected, today) && (
            <h2 className={styles.sectionLabel}>
              {selected.toLocaleDateString('en-US', { weekday: 'long', month: 'short', day: 'numeric' })}
            </h2>
          )}

        {scheduledAgenda.length === 0 && trackAgenda.length === 0
          ? activityAvailable ? <PanelEmpty>Nothing scheduled.</PanelEmpty> : null
          : (
        <div className={styles.rows}>
          {scheduledAgenda.map((event) => (
            <span key={`scheduled-${event.track.id}-${event.hour}`}>
              {renderTrackRow(event.track, {
                variant: 'panel',
                hourLabel: formatHour(event.hour),
                areaName: areaOf(event.track.areaId, areas)?.name ?? UNKNOWN_AREA,
              })}
            </span>
          ))}
          {trackAgenda.filter((track) => !scheduledIds.has(track.id)).map((track) => (
            <span key={`track-${track.id}`}>
              {renderTrackRow(track, { variant: 'panel', areaName: areaOf(track.areaId, areas)?.name ?? UNKNOWN_AREA })}
            </span>
          ))}
        </div>
          )}
      </div>
    </div>
  );
}
