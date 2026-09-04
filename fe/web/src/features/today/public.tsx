// Today — the landing route. Presentational and props-driven: the data comes
// from app/router, and navigation leaves through `onOpenTrack` (features must
// not import app).
//
// §8.1 — the header keeps the compact waiting/running summary, while the main
// column belongs wholly to the durable Today document. The clock, which used to
// be 36px, is ambient information and sits at the header's right edge at
// --text-base rather than competing with that document.
//
// #1253 D2 — "the document is the protagonist" is expressed by area and visual
// weight, and by the document region reading at the prose rank while the rest of
// the page stays interface-sized. Running remains ambience in the panel; the
// former Waiting-on-you list was removed from the reading column by owner call.

import { Calendar as AstryxCalendar, type ISODateString } from '@astryxdesign/core/Calendar';
import { useEffect, useMemo, useRef, type ReactNode } from 'react';

import {
  activeTracksOn, isRunning, needsUserAttention, visibleTracks, type Track,
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

// `ScheduledEvent`, `TrackRowRenderer` and `TodayPageProps` are declared in
// `page-props.ts`, next to the viewport ledger that has to enumerate every one
// of `TodayPageProps`' keys, and are re-exported here so this module stays the
// feature's entry. See that file's header for why the ledger cannot live here.
export type { ScheduledEvent, TodayPageProps, TrackRowRenderer } from './page-props.ts';

/** The copy for "the day has no document yet". */
const NO_PROGRESS_YET = 'Nothing written today yet.';

const SHORT_DAYS = Object.freeze(['M', 'T', 'W', 'T', 'F', 'S', 'S'] as const);

/**
 * The week grid's section label, named after the week it actually labels.
 *
 * It used to read `weekStart`'s month alone, which put `August 2026` above a
 * grid of 8/31–9/6 while the page header said `Thursday, September 3` — two
 * months on one screen, and the reader has to work out which one the calendar
 * means. A week crossing a month is ordinary (most months produce one), so this
 * is the common path, not a boundary case.
 *
 * Naming both months is what keeps the label a label: the row names the seven
 * days below it, and on a crossing week those days are in two months. It also
 * cannot contradict the header, whose month is always one of the two.
 *
 * The year is printed once when both ends share it and twice when they do not,
 * so a New Year's week reads `Dec 2026 – Jan 2027` rather than filing December
 * under the wrong year. Short month names keep the crossing label inside a
 * seven-column panel that full names would overrun.
 */
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

/**
 * An agenda row spans areas, so it always names one. When the id resolves to
 * nothing the row says so rather than going silent: a row with the area phrase
 * simply missing is indistinguishable from a row that belongs nowhere, and the
 * unresolvable case is exactly the one worth seeing.
 */
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

/**
 * The current time, and the day it falls in.
 *
 * Both viewports read the clock the same way, so it is one hook rather than two
 * copies: a pinned `nowMs` freezes it (tests assert across midnight and DST
 * without waiting), and an unpinned one ticks slowly — 15s is enough for a
 * minute-resolution clock and for the day to roll over on its own.
 *
 * It is held by each renderer rather than above them, so **crossing the
 * breakpoint resamples the clock**: the outgoing renderer unmounts, its
 * interval is cleared, and the incoming one seeds from `new Date()` and starts
 * a fresh one. That is a real change from the single-component version, where
 * the state sat above the branch and survived the switch. It is not a
 * regression — an unpinned clock that was up to 15s stale becomes current the
 * moment you resize, and a pinned `nowMs` behaves identically either way — but
 * it is a change, and the alternative costs the guarantee: hoisting the state
 * means hoisting it into the one function that must not hold both the viewport
 * bit and the props.
 */
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

/**
 * Today, as two renderers and the ledger that says which props reach which.
 *
 * The compact renderer takes `TodayCompactProps`, which is `Pick<TodayPageProps,
 * …>` over the keys `TODAY_VIEWPORT_LEDGER` marks as rendered. That is the
 * point of the split: what the phone leaves out is declared in the ledger, and
 * a prop declared as left out is *not a member of the type* the phone renderer
 * receives, so touching it does not compile. Before #1234 the phone branch sat
 * inside this function with the full prop bag in scope, and #1253 added six
 * of them that the phone never drew, with nobody the wiser.
 *
 * **This function does not know which viewport it is on**, and that is the
 * second half of the guarantee rather than a stylistic choice. While it held
 * both the viewport bit and the full props, an `if (compact) return <>{
 * props.launchpadDocument}…</>` compiled clean — the ledger said the phone
 * does not draw that prop and the compiler had no opinion, because the read
 * happened out here rather than inside `TodayCompact`. `ViewportDispatch` now
 * holds the bit and is generic in both prop packs, so it cannot name a field;
 * this function can name fields but cannot tell the viewports apart. The prop
 * packs it builds are the only channel *while that stays true*, and
 * `compactProps` is typed `TodayCompactProps`, so anything the ledger excludes
 * is an excess property. Re-importing `useCompactViewport` into this file
 * re-opens the hole and compiles — that is the acknowledged residual, and it
 * is an import in the header rather than a line hidden in a branch.
 */
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

/*
 * The two renderers' real parameter types, exported so the ledger's contract
 * test can pin them from *outside* this file.
 *
 * Asserting it in here would be worth less than it looks: a local
 * `type TodayPageProps = …` shadowing the import satisfies any assertion
 * written against the bare name, which is one of the bypasses review measured
 * as green. These aliases are derived from the functions themselves, so the
 * comparison against the canonical types happens in a module where the names
 * cannot be shadowed. See `page-props.test.ts`.
 */
export type TodayPageSignature = Parameters<typeof TodayPage>[0];
export type TodayCompactSignature = Parameters<typeof TodayCompact>[0];

/**
 * The phone: a header and the month calendar.
 *
 * Its props type is the ledger's `render: true` half, so the list of things it
 * *could* draw is exactly the list the ledger claims. Everything else Today
 * receives is unreachable from in here, by construction and not by convention.
 */
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
  documentAction,
}: TodayPageProps) {
  const { now, today } = useNow(nowMs);

  const shownTracks = visibleTracks(tracks);
  const waiting = shownTracks.filter(needsUserAttention);
  const running = shownTracks.filter((track) => isRunning(track.lifecycle) && !needsUserAttention(track));
  const panel = (
    <aside className={styles.panelColumn} data-nc-panel="">
      <PanelCard>
        <PanelModule title="Calendar">
          <Calendar
            today={today}
            tracks={shownTracks}
            areas={areas}
            scheduledEvents={scheduledEvents}
            renderTrackRow={renderTrackRow}
            nowMs={now.getTime()}
          />
        </PanelModule>
        <PanelRows title="Running" tracks={running} render={renderTrackRow} />
        <PanelModule title="Conversations" action={conversationAction}>{conversationList}</PanelModule>
      </PanelCard>
    </aside>
  );
  return (
    <div className={styles.page}>
      <TodayHeader
        today={today} waiting={waiting.length} running={running.length}
        now={now}
      />
      <div className={styles.content}>
        {/* The reading column is the document and nothing else. A dashed
            terminal placeholder and the Waiting-on-you list previously shared
            it; both are retired by owner call. */}
        <div className={styles.mainColumn}>
          <TodayDocument
            launchpad={launchpad}
            document={launchpadDocument}
            error={launchpadError}
            action={documentAction}
          />
        </div>

        {/*
          One card, two modules — the skeleton the Today and Track routes share. The
          route-specific module comes first because it is why you are on this
          route; the conversation list is second and identical everywhere, so
          it can be found without reading it.

          The conversation module was proposed for removal on 2026-09-03 and
          kept; #1341 then changed its source. On Today it is now the launchpad
          Track's own list, and rows open in place. See
          `TodayPageProps.conversationList`.
        */}
        {/* `data-nc-panel` is how `app/shell` hides this while the conversation
            drawer is open: the drawer is a card on this exact track, and a
            panel left under it shows as a sliver along its edges. A local CSS
            Module class is not nameable from the shell's stylesheet, so the
            marker is the seam.

            Today needs it more than the track page does, not less. That panel
            is sticky at the same offset the drawer starts at, so it stays
            behind it; this column is not sticky, so it scrolls up out from
            under the drawer's top edge and would surface above it. */}
        {panel}
      </div>
    </div>
  );
}

/**
 * The document region: the day's report, or the reason there is none.
 *
 * The order of the three branches is the invariant (§5.2). An error must not
 * fall through into the empty state (INV-TODAYDOC-002), and the empty state is
 * decided by the server's `report_has_noninitial_content` and by nothing else
 * on this page (INV-TODAYDOC-003) — no null-check of the document, no reading
 * of its text.
 */
function TodayDocument({ launchpad, document, error, action }: {
  launchpad?: TodayLaunchpadWire | null;
  document?: ReactNode;
  error?: ReactNode;
  action?: ReactNode;
}) {
  if (error !== undefined && error !== null) return <>{error}</>;
  // The read is still in flight. Not the empty state: "we do not know yet" and
  // "there is nothing" are different answers, and flashing the second one
  // while the first is true is how a page teaches people to distrust it.
  if (launchpad === undefined) return null;
  const written = launchpad !== null && launchpad.report_has_noninitial_content;
  /*
   * The wrapper is what makes the day's report read as the protagonist.
   *
   * §8.1 says weight and area, not type size — and this is the one surface
   * where the report IS the page rather than a card on it, so it carries a
   * reading measure and body type one step up from the ambience around it.
   * Owner call, 2026-09-03.
   */
  if (!written) {
    /*
     * The empty day is one sentence, centred and set in the report's own face.
     *
     * Not a box and not a top-left line of hint text: this sentence stands
     * where the document will stand, so it is the document's typography — serif,
     * one rank up — placed in the middle of the space the document would fill.
     * A dashed frame used to be here; it drew a container around a sentence
     * whose entire content is that there is no container yet.
     *
     * Nothing else is in this branch. A `Write today's progress` /
     * `Rewrite today's progress` button used to be, and it was removed on owner
     * call (#1343): the day's activity now reaches an agent when a
     * conversation is started on the launchpad, injected server-side, so the
     * button was no longer the only route to anything. The action slot is not
     * offered here either — there is nothing to reset when the report is
     * already canonical.
     */
    return (
      <div className={`${styles.document} ${styles.documentVacant}`}>
        <p className={styles.documentEmpty}>{NO_PROGRESS_YET}</p>
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

/**
 * A track list as a panel module, rendered only when it has rows.
 *
 * `variant: 'compact'` — the same rows this list has always been, moved from
 * the main column into the panel and nothing else. The `panel` variant is the
 * *agenda's*, and it is what `app/router` keys the row's delete affordance off;
 * handing it to Running would put a second Delete button on every track that is
 * also on today's agenda, in the same card.
 */
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

function TodayHeader({ today, waiting, running, now }: {
  today: Date;
  waiting: number;
  running: number;
  now: Date;
}) {
  return (
    <PageHeader
      // One row only: Today is the root, so there is no breadcrumb, and it has
      // no machine identifier. --header-h is 32.
      title={
        <PageTitle>
          {today.toLocaleDateString('en-US', { weekday: 'long', month: 'long', day: 'numeric' })}
        </PageTitle>
      }
      meta={
        <span className={styles.counts}>
          {/* The two numbers summarise the two attention sections, so they take
              the weight; the words stay quiet. */}
          <span className={styles.countValue}>{waiting}</span>
          <span className={styles.countWord}>waiting</span>
          <span className={styles.countSep} aria-hidden="true">·</span>
          <span className={styles.countValue}>{running}</span>
          <span className={styles.countWord}>running</span>
        </span>
      }
      actions={<Clock now={now} />}
    />
  );
}

/** Ambient, so position is its entire signal. No seconds — a digit changing
 *  once a second in the corner of a page people read is motion for nothing. */
function Clock({ now }: { now: Date }) {
  const hours = now.getHours();
  return (
    <span className={styles.clock}>
      {`${(hours + 11) % 12 + 1}:${String(now.getMinutes()).padStart(2, '0')} ${hours >= 12 ? 'PM' : 'AM'}`}
    </span>
  );
}

function Calendar({ today, tracks, areas, scheduledEvents, renderTrackRow, nowMs }: {
  today: Date;
  tracks: readonly Track[];
  areas: readonly Area[];
  scheduledEvents: readonly ScheduledEvent[];
  renderTrackRow: TrackRowRenderer;
  nowMs?: number;
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
  // INV-TODAY-002 — live track activity is computed independently of the
  // scheduled list; both render into the same agenda below, as the same row.
  const trackAgenda = activeTracksOn(tracks, selected, now);
  const scheduledIds = new Set(scheduledAgenda.map((event) => event.track.id));

  return (
    /*
      The calendar's five blocks used to be a bare fragment dropped straight
      into the panel body, which is `display: flex` with no `gap` — so month
      row, day names, week grid and agenda all stacked at zero distance and the
      day dots, which sit 1px off each cell's bottom edge, nearly touched the
      line below. Nothing here was "too tight" relative to anything else,
      because there was no scale at all.

      Same optical-distance rule as the rail: a label sits 4 from the thing it
      names, and a structural break is 12. The week is one group (month row
      labels it, day names label the grid), the agenda is the next.
    */
    <div className={styles.calendar}>
      <div className={styles.week}>
        {/* The calendar is not inside a panel of its own: the month row *is* its
            section label. Wrapping it would cost another 8px of padding on each
            side and drop every column from 42px to 40. */}
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
                /* The count belongs in here, not only in the glyph beside the
                   date: the mark is a superscript annotation and is hidden from
                   assistive tech, so this is the only route to "how much is on
                   Thursday?" for anyone not reading it by eye. */
                aria-label={day.toLocaleDateString('en-US', { weekday: 'long', month: 'short', day: 'numeric' })
                  + (seen.size === 0 ? '' : `, ${seen.size} track${seen.size === 1 ? '' : 's'}`)}
                onClick={() => setSelected(day)}
              >
                <span className={styles.dayNumber}>{day.getDate()}</span>
                {/*
                  A count, in hint tone, not a coloured dot.

                  The dot was one mark for any number of tracks, coloured by
                  whichever area happened to sort first — so it answered "whose
                  is the first one?", which nobody asks, while the question you
                  scan a week for ("how much is on Thursday?") went unanswered.
                  Seven coloured dots across a 244px week were also the densest
                  colour on the page, carrying the least information on it.

                  It sits beside the date rather than under it: stacked, the
                  two numerals read as two calendar facts competing, and a cell
                  showing "11" over "3" is asking you which one is the date.
                */}
                {seen.size > 0 && (
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
          {/*
            No label when the selection is today. It said "Today" directly under
            a grid whose today cell is already weighted and filled, on a page
            whose header already reads "Monday, August 10" in full — the third
            statement of one fact, and the one closest to the day dots.

            It stays for every other day, where it is the only place the date is
            written out: the grid shows that day as a bare "13".
          */}
          {!sameDay(selected, today) && (
            <h2 className={styles.sectionLabel}>
              {selected.toLocaleDateString('en-US', { weekday: 'long', month: 'short', day: 'numeric' })}
            </h2>
          )}

        {/* Empty *or* rows, never an empty `.rows` around the empty line.
            `.rows` bleeds 4px into the card's padding so a row's own fill
            padding puts its state dot on the container's inset; `PanelEmpty`
            has no such padding, so nesting it there started the sentence 4px
            left of the module title and of the conversation module's identical
            empty line. Sharing the carrier only unifies these two lines if
            they also sit at the same inset.

            `PanelEmpty` rather than a local class that merely looks like one:
            this module's empty line and the conversation module's are the same
            statement in the same card, and they had drifted apart — this one
            carried a dashed frame, which drew a box around "there is nothing
            here" and made an empty day read as a broken widget.

            The condition is *both* sources empty. The string is the one the
            live contract pins; it satisfies §5.3 as well as any rewrite. */}
        {scheduledAgenda.length === 0 && trackAgenda.length === 0
          ? <PanelEmpty>Nothing scheduled.</PanelEmpty>
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
