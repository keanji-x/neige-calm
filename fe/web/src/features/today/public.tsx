// Today — the landing route. Presentational and props-driven: the data comes
// from app/router, and navigation leaves through `onOpenTrack` (features must
// not import app).
//
// §8.1 — the user opens this to answer one question: *is anything waiting for
// me?* That question owns the top of the main column, and it is answered by
// position plus the only --warn pixels on the page — never by type size. The
// clock, which used to be 36px, is ambient information and now sits at the
// header's right edge at --text-base: a page whose job is "what needs me" cannot
// have a clock as its main emphasis.
//
// #1253 D2/D7 — the main column is now **the status bar, then the document**.
// The status bar is `N waiting · N running` in the header plus the compact
// waiting rows; it is O(1) in height, so it cannot grow and push the document
// off the first screen. "The document is the protagonist" is expressed by area
// and visual weight, and by the document region reading at the prose rank while
// the rest of the page stays interface-sized. Running moved into the panel: it
// is ambience, and the reading column belongs to the document.

import { Calendar as AstryxCalendar, type ISODateString } from '@astryxdesign/core/Calendar';
import { useEffect, useMemo, useRef, type ReactNode } from 'react';

import {
  activeTracksOn, isRunning, needsUserAttention, visibleTracks, type Track,
} from '../../../../core/domain/track.ts';
import { areaOf, type Area } from '../../../../core/domain/area.ts';
import type { TodayLaunchpadWire } from '../../../../core/domain/today.ts';
import { PageHeader, PageTitle } from '../../ui/page-header/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import { PanelCard, PanelEmpty, PanelModule } from '../../ui/panel-card/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import styles from './today.module.css';

/**
 * INV-TODAY-002 — an hour-bucketed scheduled event.
 *
 * The list of these is **permanently empty today, and that is a seam, not dead
 * code**. The mockup carried a synthetic `SURF_SCHEDULE` keyed on hand-written
 * track ids; under real kernel data those ids never appear, so hour-scheduled
 * events stay empty until a scheduling plugin lands (drop-in: derive
 * `ScheduledEvent[]` from overlays where `kind === 'scheduled'`).
 *
 * The two agenda sources — scheduled events and live track activity — **must
 * co-exist**: a day with scheduled work *and* an open track shows both, rather
 * than letting the schedule layer monopolise the surface. That is why this
 * arrives as a prop with an empty default instead of a hard-coded `[]` inside
 * the component: deleting the branch would silently delete the seam, and the
 * contract test feeds a synthetic event through it to prove it still works.
 */
export type ScheduledEvent = Readonly<{ track: Track; date: Date; hour: number }>;

/**
 * How Today draws one track. It is injected rather than imported because the row
 * belongs to `features/track` and a feature domain may not import a sibling;
 * `app/router` supplies it, the same way it composes the area page's list. The
 * variant vocabulary is §6.3's, so every surface still renders the one row.
 */
export type TrackRowRenderer = (
  track: Track,
  options: Readonly<{ variant: 'compact' | 'panel'; hourLabel?: string; areaName?: string }>,
) => ReactNode;

/** The copy for "the day has no document yet". */
const NO_PROGRESS_YET = 'Nothing written today yet.';

/**
 * The trigger's two labels.
 *
 * Two, because re-running is the ordinary case rather than a recovery: the
 * report's own contract is "a snapshot of now, REWRITTEN every time", so a day
 * gets summarised again whenever more has happened. A single "Write" label on a
 * page that already shows a report would read as "write a second one".
 *
 * The control is NOT suppressed once a report exists.
 * `report_has_noninitial_content` is a statement about the report's current
 * text and consults no history — restoring the document to its canonical
 * skeleton flips it back to false — so using it to decide "the summary has
 * already run" would be wrong in both directions, and using it to hide the
 * re-run would silently disable the button for anyone who edited by hand.
 */
const WRITE_SUMMARY = 'Write today\u2019s progress';
const REWRITE_SUMMARY = 'Rewrite today\u2019s progress';

export type TodayPageProps = Readonly<{
  tracks: readonly Track[];
  areas: readonly Area[];
  /**
   * The launchpad resolve (`GET /api/today/launchpad`), §5.1.
   *
   * `undefined` while the read is in flight, `null` when the server answered
   * "there is no launchpad yet" — a 200 with a null body, which is an empty
   * state and not a failure.
   *
   * **`report_has_noninitial_content` is the empty-state predicate, and it is
   * the server's** (INV-TODAYDOC-003). Nothing on this page may re-derive it
   * from the document: the kernel's freshly-minted report is a well-formed
   * document carrying the maintenance-contract comment and four empty H1s, so
   * `readTrackReport` returns non-null for it and a null-check here would
   * render four empty headings where the empty state belongs. Matching on the
   * body text would be worse — it is mirror code for a body the kernel owns.
   */
  launchpad?: TodayLaunchpadWire | null;
  /**
   * The launchpad's report, already rendered. Injected rather than imported
   * for the same reason `renderTrackRow` is: `ReportDocument` is
   * `features/report` and a feature domain may not import a sibling.
   */
  launchpadDocument?: ReactNode;
  /**
   * A failure of the resolve, already rendered as an error.
   *
   * INV-TODAYDOC-002 — when this is present the document region shows it and
   * **not** the empty state. A 5xx quietly turning into "nothing written
   * today" would tell the reader their day was empty when in fact the server
   * could not be reached.
   */
  launchpadError?: ReactNode;
  /**
   * Ask the server to write today's progress (#1253 D5), or `undefined` when
   * the composition layer has no trigger to offer.
   *
   * **The control is shown whether or not anything happened today, and that is
   * deliberate.** The design's gate lives on the server: `POST
   * /api/today/summary` computes the day's activity itself and refuses an empty
   * window, creating no conversation and sending no message
   * (INV-TODAYDOC-007). This page cannot make the same decision — there is no
   * read that would tell it, by design (D4 deleted the layer that would have
   * offered one) — so hiding the button here would be a guess, and a guess that
   * is wrong in the direction that makes the feature look broken. The refusal
   * comes back as `summaryNotice` and reads as a fact about the day.
   */
  onWriteSummary?: () => void;
  /** The trigger is in flight: the control says so and does not fire again. */
  summaryPending?: boolean;
  /**
   * What the last trigger said, when it did not simply work — already worded
   * and rendered by the composition layer, the same way `launchpadError` is.
   *
   * It sits beside the button rather than replacing the document: a refused or
   * failed trigger changes nothing about the report already on screen, and
   * swapping the document out for an error would claim otherwise.
   */
  summaryNotice?: ReactNode;
  /** Navigation lives inside the injected row; Today itself opens nothing. */
  renderTrackRow: TrackRowRenderer;
  /** See INV-TODAY-002. Production passes nothing; there is no scheduler yet. */
  scheduledEvents?: readonly ScheduledEvent[];
  /**
   * The panel card's second module, composed by `app/router`.
   *
   * A slot rather than props for the same reason `renderTrackRow` is a callback:
   * `features/**` may not import a sibling domain, and the conversation list is
   * `features/chat`. The same slot appears on all three routes — that identical
   * second module is the point of the skeleton.
   *
   * It reads as a duplicate of the track pages and is not one: on Today this is
   * the **cross-track index** (#1189 S5). It is the only place a track's
   * conversations stay reachable after you navigate away from that track, and
   * G6 opens one from here — the row navigates to the track *and* opens its
   * assistant drawer. `app/router/track-conversation.test.tsx` owns that
   * contract; dropping the module turns 18 of its assertions red.
   */
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  /** Tests pin "now" so assertions cannot drift across midnight or DST. */
  nowMs?: number;
}>;

const SHORT_DAYS = Object.freeze(['M', 'T', 'W', 'T', 'F', 'S', 'S'] as const);

/**
 * An agenda row spans areas, so it always names one. When the id resolves to
 * nothing the row says so rather than going silent: a row with the area phrase
 * simply missing is indistinguishable from a row that belongs nowhere, and the
 * unresolvable case is exactly the one worth seeing.
 */
const UNKNOWN_AREA = 'Unknown area';

/**
 * How many waiting rows the status bar draws before it stops growing.
 *
 * D7 puts the status bar above the document **because it is O(1) in height**,
 * and that is the whole load-bearing reason for the order: a bar that grew with
 * the workspace would push the document off the first screen, which is the one
 * thing the layout exists to prevent. `waiting` has no natural bound — every
 * blocked track in every area lands in it — so without a cap that property is
 * simply false, and a review found it false with 100 blocked tracks.
 *
 * The overflow is not dropped. It sits behind one inert-until-clicked control,
 * so the *loaded* page is bounded while every waiting track stays reachable:
 * these rows are not repeated in the panel's RUNNING module (it excludes
 * anything already counted as waiting), so hiding them outright would make them
 * unreachable from this page. The calendar's agenda does list them, but only
 * for the selected day and without the attention framing, so it is not a
 * substitute for the bar.
 */
const WAITING_ROW_LIMIT = 5;

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

export function TodayPage({
  tracks, areas, renderTrackRow, scheduledEvents = [], conversationList, conversationAction,
  launchpad, launchpadDocument, launchpadError, nowMs,
  onWriteSummary, summaryPending, summaryNotice,
}: TodayPageProps) {
  const [now, setNow] = useState<Date>(() => (nowMs === undefined ? new Date() : new Date(nowMs)));
  const compact = useCompactViewport();

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

  if (compact) {
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

  const shownTracks = visibleTracks(tracks);
  const waiting = shownTracks.filter(needsUserAttention);
  const running = shownTracks.filter((track) => isRunning(track.lifecycle) && !needsUserAttention(track));

  /*
   * A brand-new workspace: one hero line, and *still the document*.
   *
   * `areas` is the user-visible list — #175 filters the system area out of
   * `GET /api/areas`, and the launchpad track lives in the system area. So
   * "no areas and no tracks" does NOT mean "no Today report": a workspace whose
   * only content is the day's report lands exactly here, and returning early
   * with just the hero made that report invisible and swallowed a failed
   * resolve along with it.
   */
  if (tracks.length === 0 && areas.length === 0) {
    return (
      <div className={styles.page}>
        <TodayHeader
          today={today} waiting={waiting.length} running={running.length}
          now={now}
        />
        <div className={styles.emptyPage}>
          <p className={styles.hero}>Nothing here yet.</p>
          <TodayDocument
            launchpad={launchpad}
            document={launchpadDocument}
            error={launchpadError}
            onWriteSummary={onWriteSummary}
            pending={summaryPending}
            notice={summaryNotice}
          />
        </div>
      </div>
    );
  }

  return (
    <div className={styles.page}>
      <TodayHeader
        today={today} waiting={waiting.length} running={running.length}
        now={now}
      />
      <div className={styles.content}>
        {/* Decision first, ambience after. Every row follows its content.

            A dashed "Today terminal" placeholder used to close this column and
            the README carried a whole unbuilt contract for it (INV-TODAYTERM-*).
            Neither shipped; both are retired rather than left as a promise the
            page keeps making. Owner call, 2026-09-03. */}
        <div className={styles.mainColumn}>
          {/* The status bar. An empty section renders nothing at all — no
              label, no dashed box. The absence is the message. */}
          <WaitingSection tracks={waiting} render={renderTrackRow} />

          {/* …and the document immediately after it. */}
          <TodayDocument
            launchpad={launchpad}
            document={launchpadDocument}
            error={launchpadError}
            onWriteSummary={onWriteSummary}
            pending={summaryPending}
            notice={summaryNotice}
          />
        </div>

        {/*
          One card, two modules — the skeleton every route now shares. The
          route-specific module comes first because it is why you are on this
          route; the conversation list is second and identical everywhere, so
          it can be found without reading it.

          The conversation module was proposed for removal on 2026-09-03 as a
          duplicate of the track pages' and kept: on Today it is the
          cross-track index, and it is where G6 opens an assistant conversation
          from. See `TodayPageProps.conversationList`.
        */}
        {/* `data-nc-panel` is how `app/shell` hides this while the conversation
            drawer is open: the drawer is a card on this exact track, and a
            panel left under it shows as a sliver along its edges. A local CSS
            Module class is not nameable from the shell's stylesheet, so the
            marker is the seam.

            Today needs it more than the area and track pages do, not less. Their
            panels are sticky at the same offset the drawer starts at, so they
            stay behind it; this column is not sticky, so it scrolls up out from
            under the drawer's top edge and would surface above it. */}
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
            {/* Ambience, moved out of the reading column (#1253 D7). Follows
                §6.1's rule that a section with zero rows is not rendered, which
                is what `Section` already does in the main column — an empty
                RUNNING module would read as a gap.

                RECENT used to sit here and no longer does. The calendar's
                agenda above it is `activeTracksOn(selected)` — every track
                whose lifetime overlaps the selected day, uncapped — so on the
                default selection (today) RECENT was a sorted subset of the
                list directly above it, and every non-waiting, non-running
                track alive today was drawn twice in one card. The de-dup that
                was here only excluded waiting/running; it never looked at the
                agenda. */}
            <PanelRows title="Running" tracks={running} render={renderTrackRow} />
            <PanelModule title="Conversations" action={conversationAction}>{conversationList}</PanelModule>
          </PanelCard>
        </aside>
      </div>
    </div>
  );
}

/**
 * The status bar's waiting rows, bounded.
 *
 * Collapsed it draws at most `WAITING_ROW_LIMIT` rows plus one control, so its
 * height does not depend on how much is waiting — which is what makes D7's
 * "status bar first" ordering safe for the document below it. Expanding is an
 * explicit act by a reader who has decided the list is what they came for.
 *
 * The control is a `<button>`, not a link: this surface emits no `<a href>`
 * anywhere (INV-A11Y-061), and it navigates nowhere — it reveals rows that are
 * already on this page.
 */
function WaitingSection({ tracks, render }: {
  tracks: readonly Track[];
  render: TrackRowRenderer;
}) {
  const [expanded, setExpanded] = useState(false);
  const rowsId = 'today-waiting-rows';
  if (tracks.length === 0) return null;
  const hidden = tracks.length - WAITING_ROW_LIMIT;
  const shown = expanded ? tracks : tracks.slice(0, WAITING_ROW_LIMIT);
  return (
    <section className={styles.section}>
      <h2 className={styles.sectionLabel}>Waiting on you</h2>
      {/* `aria-controls` names what `aria-expanded` is talking about: without
          it the control announces a state with no referent, and a screen
          reader cannot jump to what just appeared. Today renders one of these
          per page, so a constant id is not a collision risk. */}
      <div className={styles.rows} id={rowsId}>
        {shown.map((track) => (
          <span key={track.id}>{render(track, { variant: 'compact' })}</span>
        ))}
      </div>
      {hidden > 0 && (
        <button
          type="button"
          data-nc-action="tertiary"
          className={styles.moreButton}
          aria-expanded={expanded}
          aria-controls={rowsId}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded ? 'Show fewer' : `+${hidden} more waiting`}
        </button>
      )}
    </section>
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
function TodayDocument({ launchpad, document, error, onWriteSummary, pending, notice }: {
  launchpad?: TodayLaunchpadWire | null;
  document?: ReactNode;
  error?: ReactNode;
  onWriteSummary?: () => void;
  pending?: boolean;
  notice?: ReactNode;
}) {
  if (error !== undefined && error !== null) return <>{error}</>;
  // The read is still in flight. Not the empty state: "we do not know yet" and
  // "there is nothing" are different answers, and flashing the second one
  // while the first is true is how a page teaches people to distrust it.
  //
  // The trigger is inside this branch too, and not beside it: a control that
  // rewrites a document nobody has read yet is offered before the page knows
  // whether there IS a document, and its label would have to guess.
  if (launchpad === undefined) return null;
  const written = launchpad !== null && launchpad.report_has_noninitial_content;
  const trigger = (
    <SummaryTrigger
      label={written ? REWRITE_SUMMARY : WRITE_SUMMARY}
      onWrite={onWriteSummary}
      pending={pending}
      notice={notice}
    />
  );
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
     * The empty day is centred and set in the report's own face.
     *
     * Not a box and not a top-left line of hint text: this sentence stands
     * where the document will stand, so it is the document's typography — serif,
     * one rank up — placed in the middle of the space the document would fill.
     * A dashed frame used to be here; it drew a container around a sentence
     * whose entire content is that there is no container yet.
     */
    return (
      <div className={`${styles.document} ${styles.documentVacant}`}>
        <p className={styles.documentEmpty}>{NO_PROGRESS_YET}</p>
        {trigger}
      </div>
    );
  }
  return <div className={styles.document}>{document}{trigger}</div>;
}

/**
 * The "write today's progress" control, plus whatever the last attempt said.
 *
 * A `<button>`, like every other control on this surface: it emits no
 * `<a href>` (INV-A11Y-061) and it navigates nowhere.
 *
 * `undefined` `onWrite` renders nothing at all rather than a disabled control.
 * A disabled button is a promise that it will work later; an absent one is the
 * honest shape for "this composition has no trigger", which is what the
 * feature's own suites pass.
 */
function SummaryTrigger({ label, onWrite, pending, notice }: {
  label: string;
  onWrite?: () => void;
  pending?: boolean;
  notice?: ReactNode;
}) {
  if (onWrite === undefined) return null;
  const busy = pending === true;
  return (
    <div className={styles.summaryTrigger}>
      <button
        type="button"
        data-nc-action="tertiary"
        className={styles.moreButton}
        // Disabled only while a request is actually in flight, so a double
        // click cannot send two. Not a general "can you press this?" gate:
        // whether there is anything to summarise is the server's answer.
        disabled={busy}
        aria-busy={busy}
        onClick={onWrite}
      >
        {busy ? 'Writing\u2026' : label}
      </button>
      {notice !== undefined && notice !== null && <>{notice}</>}
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
            {weekStart.toLocaleDateString('en-US', { month: 'long', year: 'numeric' })}
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

        <div className={styles.rows}>
          {/* Only when *both* sources are empty. The string is the one the live
              contract pins; it satisfies §5.3 as well as any rewrite would. */}
          {scheduledAgenda.length === 0 && trackAgenda.length === 0 && (
            /* `PanelEmpty`, not a local class that merely looks like it. This
               module's empty line and the conversation module's are the same
               statement in the same card, and they had drifted apart: this one
               carried a dashed frame, which drew a box around "there is nothing
               here" and made an empty day read as a broken widget. Sharing the
               carrier is what stops them drifting again. */
            <PanelEmpty>Nothing scheduled.</PanelEmpty>
          )}
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
      </div>
    </div>
  );
}
