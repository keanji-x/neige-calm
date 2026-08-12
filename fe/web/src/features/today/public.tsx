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

import { useEffect, useMemo, useRef, type ReactNode } from 'react';

import {
  activeWavesOn, isRunning, needsUserAttention, type Wave,
} from '../../../../core/domain/wave.ts';
import { coveOf, type Cove } from '../../../../core/domain/cove.ts';
import { PageHeader, PageTitle } from '../../ui/page-header/public.tsx';
import { PanelCard, PanelModule } from '../../ui/panel-card/public.tsx';
import { useState } from '../../ui/state/public.ts';
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

export type TodayPageProps = Readonly<{
  waves: readonly Wave[];
  coves: readonly Cove[];
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
  pageTitleRef?: React.RefObject<HTMLElement | null>;
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

export function TodayPage({
  waves, coves, renderWaveRow, scheduledEvents = [], conversationList, conversationAction,
  pageTitleRef, nowMs,
}: TodayPageProps) {
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

  const waiting = waves.filter(needsUserAttention);
  const running = waves.filter((wave) => isRunning(wave.lifecycle) && !needsUserAttention(wave));
  // RECENT shares the same wave list — no second request — and excludes anything
  // already shown above: one wave appearing twice on a page distorts both the
  // counts and the scan.
  const shown = new Set([...waiting, ...running].map((wave) => wave.id));
  const recent = waves
    .filter((wave) => wave.archivedAt === null && !shown.has(wave.id))
    .toSorted((left, right) => right.updatedAt - left.updatedAt)
    .slice(0, RECENT_LIMIT);


  // A brand-new workspace: one hero line and one primary action, nothing else.
  if (waves.length === 0 && coves.length === 0) {
    return (
      <div className={styles.page}>
        <TodayHeader
          today={today} waiting={waiting.length} running={running.length}
          pageTitleRef={pageTitleRef} now={now}
        />
        <div className={styles.emptyPage}>
          <p className={styles.hero}>Nothing here yet.</p>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.page}>
      <TodayHeader
        today={today} waiting={waiting.length} running={running.length}
        pageTitleRef={pageTitleRef} now={now}
      />
      <div className={styles.content}>
        {/* Decision first, ambience after: the two attention sections take as
            much height as they need, the terminal takes a fixed 240 slot, and
            RECENT absorbs whatever is left. */}
        <div className={styles.mainColumn}>
          {/* An empty section renders nothing at all — no label, no dashed box.
              The absence is the message. */}
          <Section title="Waiting on you" waves={waiting} render={renderWaveRow} />
          <Section title="Running" waves={running} render={renderWaveRow} />

          <section className={styles.terminalSlot} aria-label="Today terminal">
            <p className={styles.slotNote}>Terminal is not wired up yet.</p>
          </section>

          <Section title="Recent" waves={recent} render={renderWaveRow} />
        </div>

        {/*
          One card, two modules — the skeleton every route now shares. The
          route-specific module comes first because it is why you are on this
          route; the conversation list is second and identical everywhere, so
          it can be found without reading it.
        */}
        <aside className={styles.panelColumn}>
          <PanelCard>
            <PanelModule title="Calendar">
              <Calendar
                today={today}
                waves={waves}
                coves={coves}
                scheduledEvents={scheduledEvents}
                renderWaveRow={renderWaveRow}
                nowMs={now.getTime()}
              />
            </PanelModule>
            <PanelModule title="Conversations" action={conversationAction}>{conversationList}</PanelModule>
          </PanelCard>
        </aside>
      </div>
    </div>
  );
}

function TodayHeader({ today, waiting, running, pageTitleRef, now }: {
  today: Date;
  waiting: number;
  running: number;
  pageTitleRef?: React.RefObject<HTMLElement | null>;
  now: Date;
}) {
  return (
    <PageHeader
      // One row only: Today is the root, so there is no breadcrumb, and it has
      // no machine identifier. --header-h is 32.
      title={
        <PageTitle titleRef={pageTitleRef as React.RefObject<HTMLHeadingElement | null>}>
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

function Section({ title, waves, render }: {
  title: string;
  waves: readonly Wave[];
  render: WaveRowRenderer;
}) {
  if (waves.length === 0) return null;
  return (
    <section className={styles.section}>
      <h2 className={styles.sectionLabel}>{title}</h2>
      <div className={styles.rows}>
        {waves.map((wave) => (
          <span key={wave.id}>{render(wave, { variant: 'compact' })}</span>
        ))}
      </div>
    </section>
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
            aria-label="Previous week" onClick={() => setSelected(addDays(selected, -7))}>‹</button>
          <span className={styles.monthLabel}>
            {weekStart.toLocaleDateString('en-US', { month: 'long', year: 'numeric' })}
          </span>
          <button type="button" data-nc-role="icon" className={styles.navButton}
            aria-label="Next week" onClick={() => setSelected(addDays(selected, 7))}>›</button>
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
