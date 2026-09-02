// Today — the landing route. Presentational and props-driven: the data comes
// from app/router, and navigation leaves through `onOpenWave` (features must
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
// and visual weight, not by type size (§8.1). Running and Recent moved into the
// panel: they are ambience, and the reading column belongs to the document.

import { Calendar as AstryxCalendar, type ISODateString } from '@astryxdesign/core/Calendar';
import { useEffect, useMemo, useRef, type ReactNode } from 'react';

import {
  activeWavesOn, isRunning, needsUserAttention, visibleWaves, type Wave,
} from '../../../../core/domain/wave.ts';
import { coveOf, type Cove } from '../../../../core/domain/cove.ts';
import type { TodayLaunchpadWire } from '../../../../core/domain/today.ts';
import { PageHeader, PageTitle } from '../../ui/page-header/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import { PanelCard, PanelModule } from '../../ui/panel-card/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import styles from './today.module.css';

/**
 * INV-TODAY-002 — an hour-bucketed scheduled event.
 *
 * The list of these is **permanently empty today, and that is a seam, not dead
 * code**. The mockup carried a synthetic `SURF_SCHEDULE` keyed on hand-written
 * wave ids; under real kernel data those ids never appear, so hour-scheduled
 * events stay empty until a scheduling plugin lands (drop-in: derive
 * `ScheduledEvent[]` from overlays where `kind === 'scheduled'`).
 *
 * The two agenda sources — scheduled events and live wave activity — **must
 * co-exist**: a day with scheduled work *and* an open wave shows both, rather
 * than letting the schedule layer monopolise the surface. That is why this
 * arrives as a prop with an empty default instead of a hard-coded `[]` inside
 * the component: deleting the branch would silently delete the seam, and the
 * contract test feeds a synthetic event through it to prove it still works.
 */
export type ScheduledEvent = Readonly<{ wave: Wave; date: Date; hour: number }>;

/**
 * How Today draws one wave. It is injected rather than imported because the row
 * belongs to `features/wave` and a feature domain may not import a sibling;
 * `app/router` supplies it, the same way it composes the cove page's list. The
 * variant vocabulary is §6.3's, so every surface still renders the one row.
 */
export type WaveRowRenderer = (
  wave: Wave,
  options: Readonly<{ variant: 'compact' | 'panel'; hourLabel?: string; coveName?: string }>,
) => ReactNode;

/**
 * The copy for "the day has no document yet".
 *
 * There is no trigger button beside it. `POST /api/today/summary` does not
 * exist until #1253 PR2, and a button that cannot do anything — stubbed,
 * mocked or disabled — is worse than its absence.
 */
const NO_PROGRESS_YET = 'Nothing written today yet.';

export type TodayPageProps = Readonly<{
  waves: readonly Wave[];
  coves: readonly Cove[];
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
   * `readWaveReport` returns non-null for it and a null-check here would
   * render four empty headings where the empty state belongs. Matching on the
   * body text would be worse — it is mirror code for a body the kernel owns.
   */
  launchpad?: TodayLaunchpadWire | null;
  /**
   * The launchpad's report, already rendered. Injected rather than imported
   * for the same reason `renderWaveRow` is: `ReportDocument` is
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
  /** Navigation lives inside the injected row; Today itself opens nothing. */
  renderWaveRow: WaveRowRenderer;
  /** See INV-TODAY-002. Production passes nothing; there is no scheduler yet. */
  scheduledEvents?: readonly ScheduledEvent[];
  /**
   * The panel card's second module, composed by `app/router`.
   *
   * A slot rather than props for the same reason `renderWaveRow` is a callback:
   * `features/**` may not import a sibling domain, and the conversation list is
   * `features/chat`. The same slot appears on all three routes — that identical
   * second module is the point of the skeleton.
   */
  conversationList?: ReactNode;
  /** The conversation module head's `+`, composed by `app/router`. */
  conversationAction?: ReactNode;
  /** Tests pin "now" so assertions cannot drift across midnight or DST. */
  nowMs?: number;
}>;

const SHORT_DAYS = Object.freeze(['M', 'T', 'W', 'T', 'F', 'S', 'S'] as const);

/**
 * An agenda row spans coves, so it always names one. When the id resolves to
 * nothing the row says so rather than going silent: a row with the cove phrase
 * simply missing is indistinguishable from a row that belongs nowhere, and the
 * unresolvable case is exactly the one worth seeing.
 */
const UNKNOWN_COVE = 'Unknown cove';

/** It answers "what happened while I was away", not "browse the archive". */
const RECENT_LIMIT = 12;

/**
 * How many waiting rows the status bar draws before it stops growing.
 *
 * D7 puts the status bar above the document **because it is O(1) in height**,
 * and that is the whole load-bearing reason for the order: a bar that grew with
 * the workspace would push the document off the first screen, which is the one
 * thing the layout exists to prevent. `waiting` has no natural bound — every
 * blocked wave in every cove lands in it — so without a cap that property is
 * simply false, and a review found it false with 100 blocked waves.
 *
 * The overflow is not dropped. It sits behind one inert-until-clicked control,
 * so the *loaded* page is bounded while every waiting wave stays reachable:
 * these rows are not repeated in the panel (RUNNING and RECENT both exclude
 * anything already counted as waiting), so hiding them outright would make
 * them unreachable from this page.
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
  waves, coves, renderWaveRow, scheduledEvents = [], conversationList, conversationAction,
  launchpad, launchpadDocument, launchpadError, nowMs,
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

  const shownWaves = visibleWaves(waves);
  const waiting = shownWaves.filter(needsUserAttention);
  const running = shownWaves.filter((wave) => isRunning(wave.lifecycle) && !needsUserAttention(wave));
  // RECENT shares the same wave list — no second request — and excludes anything
  // already shown above: one wave appearing twice on a page distorts both the
  // counts and the scan.
  const shown = new Set([...waiting, ...running].map((wave) => wave.id));
  const recent = shownWaves
    .filter((wave) => !shown.has(wave.id))
    .toSorted((left, right) => right.updatedAt - left.updatedAt)
    .slice(0, RECENT_LIMIT);


  /*
   * A brand-new workspace: one hero line, and *still the document*.
   *
   * `coves` is the user-visible list — #175 filters the system cove out of
   * `GET /api/coves`, and the launchpad wave lives in the system cove. So
   * "no coves and no waves" does NOT mean "no Today report": a workspace whose
   * only content is the day's report lands exactly here, and returning early
   * with just the hero made that report invisible and swallowed a failed
   * resolve along with it.
   */
  if (waves.length === 0 && coves.length === 0) {
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
        {/* Decision first, ambience after. Every row follows its content while
            the unwired terminal keeps its route anchor without reserving a
            full terminal-height track. */}
        <div className={styles.mainColumn}>
          {/* The status bar. An empty section renders nothing at all — no
              label, no dashed box. The absence is the message. */}
          <WaitingSection waves={waiting} render={renderWaveRow} />

          {/* …and the document immediately after it. */}
          <TodayDocument
            launchpad={launchpad}
            document={launchpadDocument}
            error={launchpadError}
          />

          <section className={styles.terminalSlot} aria-label="Today terminal">
            <p className={styles.slotNote}>Terminal is not wired up yet.</p>
          </section>
        </div>

        {/*
          One card, two modules — the skeleton every route now shares. The
          route-specific module comes first because it is why you are on this
          route; the conversation list is second and identical everywhere, so
          it can be found without reading it.
        */}
        {/* `data-nc-panel` is how `app/shell` hides this while the conversation
            drawer is open: the drawer is a card on this exact track, and a
            panel left under it shows as a sliver along its edges. A local CSS
            Module class is not nameable from the shell's stylesheet, so the
            marker is the seam.

            Today needs it more than the cove and wave pages do, not less. Their
            panels are sticky at the same offset the drawer starts at, so they
            stay behind it; this column is not sticky, so it scrolls up out from
            under the drawer's top edge and would surface above it. */}
        <aside className={styles.panelColumn} data-nc-panel="">
          <PanelCard>
            <PanelModule title="Calendar">
              <Calendar
                today={today}
                waves={shownWaves}
                coves={coves}
                scheduledEvents={scheduledEvents}
                renderWaveRow={renderWaveRow}
                nowMs={now.getTime()}
              />
            </PanelModule>
            {/* Ambience, moved out of the reading column (#1253 D7). Both
                modules follow §6.1's rule that a section with zero rows is not
                rendered, which is what `Section` already does in the main
                column — an empty RUNNING module would read as a gap. */}
            <PanelRows title="Running" waves={running} render={renderWaveRow} />
            <PanelRows title="Recent" waves={recent} render={renderWaveRow} />
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
function WaitingSection({ waves, render }: {
  waves: readonly Wave[];
  render: WaveRowRenderer;
}) {
  const [expanded, setExpanded] = useState(false);
  const rowsId = 'today-waiting-rows';
  if (waves.length === 0) return null;
  const hidden = waves.length - WAITING_ROW_LIMIT;
  const shown = expanded ? waves : waves.slice(0, WAITING_ROW_LIMIT);
  return (
    <section className={styles.section}>
      <h2 className={styles.sectionLabel}>Waiting on you</h2>
      {/* `aria-controls` names what `aria-expanded` is talking about: without
          it the control announces a state with no referent, and a screen
          reader cannot jump to what just appeared. Today renders one of these
          per page, so a constant id is not a collision risk. */}
      <div className={styles.rows} id={rowsId}>
        {shown.map((wave) => (
          <span key={wave.id}>{render(wave, { variant: 'compact' })}</span>
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
function TodayDocument({ launchpad, document, error }: {
  launchpad?: TodayLaunchpadWire | null;
  document?: ReactNode;
  error?: ReactNode;
}) {
  if (error !== undefined && error !== null) return <>{error}</>;
  // The read is still in flight. Not the empty state: "we do not know yet" and
  // "there is nothing" are different answers, and flashing the second one
  // while the first is true is how a page teaches people to distrust it.
  if (launchpad === undefined) return null;
  if (launchpad === null || !launchpad.report_has_noninitial_content) {
    return <p className={styles.inlineEmpty}>{NO_PROGRESS_YET}</p>;
  }
  return <>{document}</>;
}

/**
 * A wave list as a panel module, rendered only when it has rows.
 *
 * `variant: 'compact'` — the same rows these two lists have always been, moved
 * from the main column into the panel and nothing else. The `panel` variant is
 * the *agenda's*, and it is what `app/router` keys the row's delete affordance
 * off; handing it to Running and Recent would put a second Delete button on
 * every wave that is also on today's agenda, in the same card.
 */
function PanelRows({ title, waves, render }: {
  title: string;
  waves: readonly Wave[];
  render: WaveRowRenderer;
}) {
  if (waves.length === 0) return null;
  return (
    <PanelModule title={title}>
      <div className={styles.rows}>
        {waves.map((wave) => (
          <span key={wave.id}>{render(wave, { variant: 'compact' })}</span>
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

function Calendar({ today, waves, coves, scheduledEvents, renderWaveRow, nowMs }: {
  today: Date;
  waves: readonly Wave[];
  coves: readonly Cove[];
  scheduledEvents: readonly ScheduledEvent[];
  renderWaveRow: WaveRowRenderer;
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
  // INV-TODAY-002 — live wave activity is computed independently of the
  // scheduled list; both render into the same agenda below, as the same row.
  const waveAgenda = activeWavesOn(waves, selected, now);
  const scheduledIds = new Set(scheduledAgenda.map((event) => event.wave.id));

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
            // De-dup by wave id: a wave with both a scheduled event and an
            // overlapping activity window is counted once, not twice.
            const seen = new Set<string>();
            for (const event of scheduledEvents.filter((candidate) => sameDay(candidate.date, day))) {
              seen.add(event.wave.id);
            }
            for (const wave of activeWavesOn(waves, day, now)) {
              seen.add(wave.id);
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
                  + (seen.size === 0 ? '' : `, ${seen.size} wave${seen.size === 1 ? '' : 's'}`)}
                onClick={() => setSelected(day)}
              >
                <span className={styles.dayNumber}>{day.getDate()}</span>
                {/*
                  A count, in hint tone, not a coloured dot.

                  The dot was one mark for any number of waves, coloured by
                  whichever cove happened to sort first — so it answered "whose
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
          {scheduledAgenda.length === 0 && waveAgenda.length === 0 && (
            <p className={styles.inlineEmpty}>Nothing scheduled.</p>
          )}
          {scheduledAgenda.map((event) => (
            <span key={`scheduled-${event.wave.id}-${event.hour}`}>
              {renderWaveRow(event.wave, {
                variant: 'panel',
                hourLabel: formatHour(event.hour),
                coveName: coveOf(event.wave.coveId, coves)?.name ?? UNKNOWN_COVE,
              })}
            </span>
          ))}
          {waveAgenda.filter((wave) => !scheduledIds.has(wave.id)).map((wave) => (
            <span key={`wave-${wave.id}`}>
              {renderWaveRow(wave, { variant: 'panel', coveName: coveOf(wave.coveId, coves)?.name ?? UNKNOWN_COVE })}
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}
