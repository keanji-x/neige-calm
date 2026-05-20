import { lazy, Suspense, useEffect, useMemo } from 'react';
import { useState } from '../shared/state';
import { coveOf, timeOfDay } from '../shared/components/helpers';
import type { Cove, Route, Wave } from '../types';

// xterm.js is heavy and only mounts when the Today home panel resolves a
// live terminal. Splitting it lets Today's calendar / clock render before
// the terminal renderer downloads.
const XtermView = lazy(() =>
  import('../XtermView').then((m) => ({ default: m.XtermView })),
);

// ============================================================
// Calendar helpers.
//
// The mockup ported a synthetic `SURF_SCHEDULE` keyed on the design's
// hand-written wave ids (`w-001` etc); under real kernel data those ids
// never appear, so the calendar would be permanently empty. We instead
// surface a calm "Nothing scheduled." state and wait for a scheduling
// plugin to write proper overlays. Drop-in replacement when that lands:
// derive `CalEvent[]` from overlays where `kind === "scheduled"` and
// `payload = { date, hour, dur }`.
// ============================================================

const SHORT_DAYS = ['M', 'T', 'W', 'T', 'F', 'S', 'S'];

const addDays = (d: Date, n: number) => {
  const r = new Date(d);
  r.setDate(r.getDate() + n);
  return r;
};
const sameDay = (a: Date, b: Date) =>
  a.getFullYear() === b.getFullYear() &&
  a.getMonth() === b.getMonth() &&
  a.getDate() === b.getDate();
const startOfWeek = (d: Date) => {
  const r = new Date(d);
  const dow = (r.getDay() + 6) % 7;
  r.setDate(r.getDate() - dow);
  r.setHours(0, 0, 0, 0);
  return r;
};
const startOfMonth = (d: Date) => {
  const r = new Date(d.getFullYear(), d.getMonth(), 1);
  r.setHours(0, 0, 0, 0);
  return r;
};
const fmtHour = (h: number) => {
  const p = h >= 12 ? 'pm' : 'am';
  const hh = (h + 11) % 12 + 1;
  return hh + p;
};
interface CalEvent { wave: Wave; date: Date; h: number; dur: number; }

// ============================================================
// TodayPage — terminal-launcher home + calendar rail.
// Ports the design's SurfPage. The Surf vocabulary lives in CSS class
// names (.surf, .surf-clock, …) — we keep it there since it's stable.
// ============================================================

export function TodayPage({
  waves,
  coves,
  onGo,
  todayTerminalId,
  todayError,
  onResetTodayTerminal,
}: {
  waves: Wave[];
  coves: Cove[];
  onGo: (r: Route) => void;
  /** When defined, the home panel hosts a live `<XtermView>` for this id. */
  todayTerminalId?: string | null;
  todayError?: Error | null;
  onResetTodayTerminal?: () => void;
}) {
  const today0 = useMemo(() => {
    const t = new Date();
    t.setHours(0, 0, 0, 0);
    return t;
  }, []);

  // No scheduling plugin yet — the calendar renders its shell with an
  // empty list. When a plugin defines a `scheduled` overlay this is the
  // single line that should fold it in.
  const events = useMemo<CalEvent[]>(() => [], []);

  return (
    <div className="surf">
      <section className="surf-main">
        <SurfClock waves={waves} />
        <TodayTerminalPanel
          terminalId={todayTerminalId ?? null}
          error={todayError ?? null}
          onReset={onResetTodayTerminal}
        />
      </section>
      <aside className="surf-rail">
        <CalendarCard
          today0={today0}
          events={events}
          coves={coves}
          waves={waves}
          onGo={onGo}
        />
      </aside>
    </div>
  );
}

// ---------------- TodayTerminalPanel — the real default PTY on Today ----------------
//
// Replaces the original mockup's static `SurfTerminal` with an actual live
// shell. The terminal binds to a single per-browser "Scratch / Today"
// card (resolved by `useTodayTerminal` upstream and passed in as
// `terminalId`). While the resolver runs we show a calm "Booting…" line.
//
// `onReset` lets the upstream wipe the cached binding (e.g. if a future
// "kill" affordance lands), forcing a fresh bootstrap on next render.

function TodayTerminalPanel({
  terminalId,
  error,
  onReset,
}: {
  terminalId: string | null;
  error: Error | null;
  onReset?: () => void;
}) {
  return (
    <div className="surf-term">
      <div className="surf-term-head">
        <span className="term-dot" />
        <span className="term-dot b" />
        <span className="term-dot c" />
        <span className="term-title">~ / neige · today</span>
        {onReset && (
          <button
            className="surf-term-host"
            onClick={onReset}
            title="Forget the cached Today terminal and bootstrap a fresh one"
            style={{
              background: 'transparent',
              border: 'none',
              cursor: 'pointer',
              font: 'inherit',
              color: 'inherit',
              padding: 0,
            }}
          >
            reset ↻
          </button>
        )}
      </div>
      <div className="surf-term-body" style={{ padding: 0 }}>
        {error ? (
          <div className="surf-term-line" style={{ padding: 16, color: 'var(--warn, #c00)' }}>
            kernel error: {error.message}
            {onReset && (
              <>
                {' · '}
                <button
                  onClick={onReset}
                  style={{
                    background: 'none', border: 'none', padding: 0,
                    color: 'inherit', textDecoration: 'underline', cursor: 'pointer',
                    font: 'inherit',
                  }}
                >
                  retry
                </button>
              </>
            )}
          </div>
        ) : terminalId ? (
          <LiveTerminalSlot terminalId={terminalId} />
        ) : (
          <div className="surf-term-line dim" style={{ padding: 16 }}>
            booting today's terminal…
          </div>
        )}
      </div>
    </div>
  );
}

function LiveTerminalSlot({ terminalId }: { terminalId: string }) {
  return (
    <div style={{ minHeight: 360 }}>
      <Suspense fallback={<div className="synth">Loading terminal…</div>}>
        <XtermView terminalId={terminalId} />
      </Suspense>
    </div>
  );
}

// ---------------- Clock ----------------

function SurfClock({ waves }: { waves: Wave[] }) {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const id = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(id);
  }, []);

  const hh = now.getHours();
  const mm = String(now.getMinutes()).padStart(2, '0');
  const period = hh >= 12 ? 'PM' : 'AM';
  const h12 = (hh + 11) % 12 + 1;
  const weekday = now.toLocaleDateString('en-US', { weekday: 'long' });
  const datePart = now.toLocaleDateString('en-US', { month: 'long', day: 'numeric' });

  const runningCount = waves.filter((w) => w.status === 'running').length;
  const waitingCount = waves.filter((w) => w.status === 'waiting').length;

  return (
    <header className="surf-clock">
      <div className="surf-clock-time">
        <span className="surf-clock-h num">{h12}</span>
        <span className="surf-clock-colon">:</span>
        <span className="surf-clock-m num">{mm}</span>
        <span className="surf-clock-ap">{period}</span>
      </div>
      <div className="surf-clock-meta">
        <div className="surf-clock-day">{weekday}</div>
        <div className="surf-clock-date">
          {datePart} · {timeOfDay()}
        </div>
      </div>
      <div className="surf-clock-status">
        <span className="surf-stat run">
          <span className="surf-stat-dot run" />
          <span className="surf-stat-n num">{runningCount}</span>
          <span className="surf-stat-lbl">running</span>
        </span>
        <span className="surf-stat-sep">·</span>
        <span className="surf-stat warn">
          <span className="surf-stat-dot warn" />
          <span className="surf-stat-n num">{waitingCount}</span>
          <span className="surf-stat-lbl">waiting</span>
        </span>
      </div>
    </header>
  );
}

// ---------------- Calendar ----------------

function CalendarCard({
  today0,
  events,
  coves,
  onGo,
}: {
  today0: Date;
  events: CalEvent[];
  coves: Cove[];
  waves: Wave[];
  onGo: (r: Route) => void;
}) {
  const [view, setView] = useState<'week' | 'month'>('week');
  const [selected, setSelected] = useState<Date>(today0);
  const [monthCursor, setMonthCursor] = useState<Date>(() => startOfMonth(today0));

  const agenda = events
    .filter((e) => sameDay(e.date, selected))
    .sort((a, b) => a.h - b.h);

  const selLabel = sameDay(selected, today0)
    ? 'Today'
    : selected.toLocaleDateString('en-US', {
        weekday: 'long', month: 'short', day: 'numeric',
      });

  return (
    <section className="surf-card cal">
      <div className="cal-head">
        <div className="h-eyebrow">Calendar</div>
        <div className="cal-toggle">
          <button className={view === 'week' ? 'on' : ''} onClick={() => setView('week')}>
            Week
          </button>
          <button className={view === 'month' ? 'on' : ''} onClick={() => setView('month')}>
            Month
          </button>
        </div>
      </div>

      {view === 'week' ? (
        <CalWeek
          today0={today0}
          selected={selected}
          setSelected={setSelected}
          events={events}
          coves={coves}
        />
      ) : (
        <CalMonth
          today0={today0}
          selected={selected}
          setSelected={setSelected}
          monthCursor={monthCursor}
          setMonthCursor={setMonthCursor}
          events={events}
          coves={coves}
        />
      )}

      <div className="cal-agenda-head">{selLabel}</div>
      <div className="cal-agenda">
        {agenda.length === 0 && <div className="cal-empty">Nothing scheduled.</div>}
        {agenda.map((e, i) => {
          const c = coveOf(e.wave.coveId, coves);
          const isWaiting = e.wave.status === 'waiting';
          const isRunning = e.wave.status === 'running';
          return (
            <button
              key={i}
              className={
                'cal-event' +
                (isWaiting ? ' waiting' : '') +
                (isRunning ? ' running' : '')
              }
              onClick={() => onGo({ name: 'wave', id: e.wave.id })}
            >
              <span className="cal-event-time num">{fmtHour(e.h)}</span>
              <span className="cal-event-bar" style={{ background: c?.color }} />
              <span className="cal-event-body">
                <div className="cal-event-title">{e.wave.title}</div>
                <div className="cal-event-meta">
                  <span style={{ color: c?.color }}>{c?.name}</span>
                  {isWaiting && (
                    <>
                      {' · '}
                      <span className="warn-text">waiting on you</span>
                    </>
                  )}
                  {isRunning && (
                    <>
                      {' · '}
                      <span className="cal-event-run">running</span>
                    </>
                  )}
                </div>
              </span>
            </button>
          );
        })}
      </div>
    </section>
  );
}

function CalWeek({
  today0,
  selected,
  setSelected,
  events,
  coves,
}: {
  today0: Date;
  selected: Date;
  setSelected: (d: Date) => void;
  events: CalEvent[];
  coves: Cove[];
}) {
  const start = startOfWeek(selected);
  const days = Array.from({ length: 7 }, (_, i) => addDays(start, i));
  const label = days[0].getMonth() === days[6].getMonth()
    ? days[0].toLocaleDateString('en-US', { month: 'long', year: 'numeric' })
    : days[0].toLocaleDateString('en-US', { month: 'short' }) +
      ' — ' +
      days[6].toLocaleDateString('en-US', { month: 'short', year: 'numeric' });

  const shift = (n: number) => setSelected(addDays(selected, n));

  return (
    <div className="cal-week">
      <div className="cal-week-head">
        <button className="cal-nav" onClick={() => shift(-7)} aria-label="Previous week">‹</button>
        <span className="cal-month-label">{label}</span>
        <button className="cal-nav" onClick={() => shift(7)} aria-label="Next week">›</button>
      </div>
      <div className="cal-week-grid">
        {days.map((d, i) => {
          const evs = events.filter((e) => sameDay(e.date, d));
          const isToday = sameDay(d, today0);
          const isSel = sameDay(d, selected);
          return (
            <button
              key={i}
              className={
                'cal-week-day' + (isToday ? ' today' : '') + (isSel ? ' sel' : '')
              }
              onClick={() => setSelected(d)}
            >
              <div className="cal-week-dow">{SHORT_DAYS[i]}</div>
              <div className="cal-week-date">{d.getDate()}</div>
              <div className="cal-week-dots">
                {evs.slice(0, 4).map((e, j) => {
                  const c = coveOf(e.wave.coveId, coves);
                  return <span key={j} className="cal-week-dot" style={{ background: c?.color }} />;
                })}
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}

function CalMonth({
  today0,
  selected,
  setSelected,
  monthCursor,
  setMonthCursor,
  events,
  coves,
}: {
  today0: Date;
  selected: Date;
  setSelected: (d: Date) => void;
  monthCursor: Date;
  setMonthCursor: (d: Date) => void;
  events: CalEvent[];
  coves: Cove[];
}) {
  const first = startOfMonth(monthCursor);
  const gridStart = startOfWeek(first);
  const days = Array.from({ length: 42 }, (_, i) => addDays(gridStart, i));
  // Drop the trailing all-other-month week to avoid a dangling empty row.
  const monthEndDay = new Date(first.getFullYear(), first.getMonth() + 1, 0).getDate();
  const offsetIntoFirstRow = (first.getDay() + 6) % 7;
  const usedRows = Math.ceil((offsetIntoFirstRow + monthEndDay) / 7);
  const visible = days.slice(0, usedRows * 7);

  return (
    <div className="cal-month">
      <div className="cal-month-head">
        <button
          className="cal-nav"
          onClick={() =>
            setMonthCursor(new Date(monthCursor.getFullYear(), monthCursor.getMonth() - 1, 1))
          }
          aria-label="Previous month"
        >‹</button>
        <span className="cal-month-label">
          {monthCursor.toLocaleDateString('en-US', { month: 'long', year: 'numeric' })}
        </span>
        <button
          className="cal-nav"
          onClick={() =>
            setMonthCursor(new Date(monthCursor.getFullYear(), monthCursor.getMonth() + 1, 1))
          }
          aria-label="Next month"
        >›</button>
      </div>
      <div className="cal-month-dow">
        {SHORT_DAYS.map((d, i) => <span key={i}>{d}</span>)}
      </div>
      <div className="cal-month-grid">
        {visible.map((d, i) => {
          const evs = events.filter((e) => sameDay(e.date, d));
          const isToday = sameDay(d, today0);
          const isSel = sameDay(d, selected);
          const otherMonth = d.getMonth() !== monthCursor.getMonth();
          return (
            <button
              key={i}
              className={
                'cal-month-day' +
                (otherMonth ? ' dim' : '') +
                (isToday ? ' today' : '') +
                (isSel ? ' sel' : '')
              }
              onClick={() => setSelected(d)}
            >
              <span className="cal-md-n">{d.getDate()}</span>
              {evs.length > 0 && (
                <span className="cal-md-dots">
                  {evs.slice(0, 3).map((e, j) => {
                    const c = coveOf(e.wave.coveId, coves);
                    return <i key={j} style={{ background: c?.color }} />;
                  })}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
