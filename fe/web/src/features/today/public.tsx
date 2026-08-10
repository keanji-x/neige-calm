// Today — the landing route. Presentational and props-driven: the data comes
// from app/router, and navigation leaves through `onOpenWave` (features must
// not import app).

import { useEffect, useMemo } from 'react';

import {
  activeWavesOn, isRunning, isWaitingForUser, lifecycleLabel, waveDisplayTitle, type Wave,
} from '../../../../core/domain/wave.ts';
import { coveOf, type Cove } from '../../../../core/domain/cove.ts';
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

export type TodayPageProps = Readonly<{
  waves: readonly Wave[];
  coves: readonly Cove[];
  onOpenWave: (waveId: string) => void;
  /** See INV-TODAY-002. Production passes nothing; there is no scheduler yet. */
  scheduledEvents?: readonly ScheduledEvent[];
  /** Tests pin "now" so assertions cannot drift across midnight or DST. */
  nowMs?: number;
}>;

const SHORT_DAYS = Object.freeze(['M', 'T', 'W', 'T', 'F', 'S', 'S'] as const);

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

export function TodayPage({ waves, coves, onOpenWave, scheduledEvents = [], nowMs }: TodayPageProps) {
  const today = useMemo(() => {
    const start = nowMs === undefined ? new Date() : new Date(nowMs);
    start.setHours(0, 0, 0, 0);
    return start;
  }, [nowMs]);

  return (
    <div className={styles.page}>
      <TodayClock waves={waves} nowMs={nowMs} />
      <div className={styles.grid}>
        <section className={styles.card} aria-label="Today terminal">
          <h2 className={styles.cardTitle}>~ / neige · today</h2>
          <p className={styles.placeholderBody}>
            The default Today terminal lands with features/today/terminal. Its
            resolve order (cached card id → verify the terminal row → bootstrap
            only on 404) is a contract, not an implementation detail — see this
            module&apos;s README before wiring it.
          </p>
        </section>
        <aside className={styles.card} aria-label="Calendar">
          <CalendarCard
            today={today}
            waves={waves}
            coves={coves}
            scheduledEvents={scheduledEvents}
            onOpenWave={onOpenWave}
            nowMs={nowMs}
          />
        </aside>
      </div>
    </div>
  );
}

function TodayClock({ waves, nowMs }: { waves: readonly Wave[]; nowMs?: number }) {
  const [now, setNow] = useState<Date>(() => (nowMs === undefined ? new Date() : new Date(nowMs)));

  useEffect(() => {
    if (nowMs !== undefined) return;
    const id = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(id);
  }, [nowMs]);

  const hours = now.getHours();
  const running = waves.filter((wave) => isRunning(wave.lifecycle)).length;
  // Same predicate the sidebar's "Waiting on you" section uses, so the two
  // surfaces cannot disagree about what counts.
  const waiting = waves.filter((wave) => isWaitingForUser(wave.lifecycle)).length;

  return (
    <header className={styles.clock}>
      <div className={styles.time}>
        <span>{(hours + 11) % 12 + 1}</span>
        <span>:</span>
        <span>{String(now.getMinutes()).padStart(2, '0')}</span>
        <span className={styles.period}>{hours >= 12 ? 'PM' : 'AM'}</span>
      </div>
      <div className={styles.weekday}>{now.toLocaleDateString('en-US', { weekday: 'long' })}</div>
      <div className={styles.stats}>
        <span className={styles.stat}>
          <span className={`${styles.dot} ${styles.dotRunning}`} aria-hidden="true" />
          {running} running
        </span>
        <span aria-hidden="true">·</span>
        <span className={styles.stat}>
          <span className={`${styles.dot} ${styles.dotWaiting}`} aria-hidden="true" />
          {waiting} waiting
        </span>
      </div>
    </header>
  );
}

function CalendarCard({ today, waves, coves, scheduledEvents, onOpenWave, nowMs }: {
  today: Date;
  waves: readonly Wave[];
  coves: readonly Cove[];
  scheduledEvents: readonly ScheduledEvent[];
  onOpenWave: (waveId: string) => void;
  nowMs?: number;
}) {
  const [selected, setSelected] = useState<Date>(today);
  const now = nowMs ?? Date.now();
  const weekStart = startOfWeek(selected);
  const days = Array.from({ length: 7 }, (_, index) => addDays(weekStart, index));

  const scheduledAgenda = scheduledEvents
    .filter((event) => sameDay(event.date, selected))
    .toSorted((left, right) => left.hour - right.hour);
  // INV-TODAY-002 — live wave activity is computed independently of the
  // scheduled list; both render into the same agenda below.
  const waveAgenda = activeWavesOn(waves, selected, now);

  const label = sameDay(selected, today)
    ? 'Today'
    : selected.toLocaleDateString('en-US', { weekday: 'long', month: 'short', day: 'numeric' });

  return (
    <>
      <div className={styles.weekHead}>
        <button type="button" className={styles.nav} aria-label="Previous week"
          onClick={() => setSelected(addDays(selected, -7))}>‹</button>
        <span className={styles.monthLabel}>
          {weekStart.toLocaleDateString('en-US', { month: 'long', year: 'numeric' })}
        </span>
        <button type="button" className={styles.nav} aria-label="Next week"
          onClick={() => setSelected(addDays(selected, 7))}>›</button>
      </div>
      <div className={styles.weekGrid}>
        {days.map((day, index) => {
          // De-dup by wave id so a wave with both a scheduled event and an
          // overlapping activity window contributes one dot, not two.
          const seen = new Set<string>();
          const dots: { id: string; color: string | undefined }[] = [];
          for (const event of scheduledEvents.filter((candidate) => sameDay(candidate.date, day))) {
            if (seen.has(event.wave.id)) continue;
            seen.add(event.wave.id);
            dots.push({ id: event.wave.id, color: coveOf(event.wave.coveId, coves)?.color });
          }
          for (const wave of activeWavesOn(waves, day, now)) {
            if (seen.has(wave.id)) continue;
            seen.add(wave.id);
            dots.push({ id: wave.id, color: coveOf(wave.coveId, coves)?.color });
          }
          const classes = [
            styles.day,
            sameDay(day, today) ? styles.dayToday : '',
            sameDay(day, selected) ? styles.daySelected : '',
          ].filter(Boolean).join(' ');
          return (
            <button
              key={day.toDateString()}
              type="button"
              className={classes}
              aria-pressed={sameDay(day, selected)}
              aria-label={day.toLocaleDateString('en-US', { weekday: 'long', month: 'short', day: 'numeric' })}
              onClick={() => setSelected(day)}
            >
              <span className={styles.dayName} aria-hidden="true">{SHORT_DAYS[index]}</span>
              <span className={styles.dayNumber}>{day.getDate()}</span>
              <span className={styles.dayDots} data-nc-day-dots aria-hidden="true">
                {dots.slice(0, 4).map((dot) => (
                  <span key={dot.id} className={styles.dayDot} data-nc-day-dot style={{ background: dot.color }} />
                ))}
              </span>
            </button>
          );
        })}
      </div>

      <div className={styles.agendaHead}>{label}</div>
      <div className={styles.agenda}>
        {scheduledAgenda.length === 0 && waveAgenda.length === 0 && (
          <div className={styles.empty}>Nothing scheduled.</div>
        )}
        {scheduledAgenda.map((event) => (
          <AgendaRow
            key={`scheduled-${event.wave.id}-${event.hour}`}
            wave={event.wave}
            hourLabel={formatHour(event.hour)}
            coves={coves}
            onOpenWave={onOpenWave}
          />
        ))}
        {waveAgenda.map((wave) => (
          <AgendaRow key={`wave-${wave.id}`} wave={wave} coves={coves} onOpenWave={onOpenWave} />
        ))}
      </div>
    </>
  );
}

/**
 * One agenda row, shared by both sources. `hourLabel` present = scheduled
 * event (time gutter); absent = live wave activity (day-level, so no gutter,
 * and the lifecycle phrase goes on a second line).
 *
 * INV-A11Y-061 — navigation is a `<button>` + callback, never an `<a href>`.
 * The full lifecycle phrase is folded into the accessible name so the dot
 * flags stay purely decorative.
 */
function AgendaRow({ wave, hourLabel, coves, onOpenWave }: {
  wave: Wave;
  hourLabel?: string;
  coves: readonly Cove[];
  onOpenWave: (waveId: string) => void;
}) {
  const cove = coveOf(wave.coveId, coves);
  const waiting = isWaitingForUser(wave.lifecycle);
  const running = isRunning(wave.lifecycle);
  const title = waveDisplayTitle(wave.title);
  const lifecycle = lifecycleLabel(wave.lifecycle);
  const bits = [waiting ? 'waiting on you' : '', running ? 'running' : ''].filter(Boolean);
  const label = `Wave ${title}${bits.length > 0 ? `, ${bits.join(', ')}` : ''}`
    + `, ${lifecycle}, in cove ${cove?.name ?? 'Unknown cove'}`;

  return (
    <button type="button" className={styles.event} aria-label={label} onClick={() => onOpenWave(wave.id)}>
      <span className={styles.eventBar} style={{ background: cove?.color }} aria-hidden="true" />
      <span className={styles.eventBody}>
        <span className={styles.eventTitleRow}>
          {hourLabel !== undefined && <span className={styles.eventLifecycle}>{hourLabel}</span>}
          <span className={styles.eventTitle}>{title}</span>
          {waiting && <span className={`${styles.flag} ${styles.flagWaiting}`} aria-hidden="true" />}
          {running && <span className={`${styles.flag} ${styles.flagRunning}`} aria-hidden="true" />}
        </span>
        {hourLabel === undefined && (
          <span className={`${styles.eventLifecycle} ${waiting ? styles.eventLifecycleAttention : ''}`}>
            {lifecycle}
          </span>
        )}
      </span>
    </button>
  );
}
