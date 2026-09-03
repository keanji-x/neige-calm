import {
  lazy,
  Suspense,
  useEffect,
  useMemo,
} from 'react';
import { useState } from '../shared/state';
import { areaOf } from '../shared/components/helpers';
import { useTheme } from '../app/theme';
import { CardHead } from '../cards/CardHead';
import { isRunning, trackNeedsUserAttention } from '../shared/lifecycle';
import { lifecycleLabel } from '../shared/components/TrackLifecycleBadge';
import { trackDisplayTitle } from '../shared/trackTitle';
import type { Area, Route, Track } from '../types';
import { useXtermWheelTargetRef } from '../input/useXtermWheelTarget';

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
// hand-written track ids (`w-001` etc); under real kernel data those ids
// never appear, so hour-scheduled events stay an empty list until a
// scheduling plugin lands (drop-in: derive `CalEvent[]` from overlays
// where `kind === "scheduled"` and `payload = { date, hour, dur }`).
//
// Issue #250 PR 5 — until then, the rail's dots and agenda surface live
// track activity: any track whose `[createdAt, terminalAt ?? now]` window
// overlaps a calendar day shows up on that day, colour-keyed by area.
// The user can finally see "what was I working on Tuesday?" without a
// scheduling layer.
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
const startOfDay = (d: Date) => {
  const r = new Date(d);
  r.setHours(0, 0, 0, 0);
  return r;
};
const endOfDay = (d: Date) => {
  const r = new Date(d);
  r.setHours(23, 59, 59, 999);
  return r;
};
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
interface CalEvent { track: Track; date: Date; h: number; dur: number; }

/**
 * Issue #250 PR 5 — every track whose `[createdAt, terminalAt ?? nowMs]`
 * interval overlaps the local day that owns `day`. Used to drive
 * per-day area-colour dots on the week / month grids and to populate
 * the selected-day agenda when no hour-scheduled `CalEvent` is present.
 *
 * The predicate uses inclusive endpoints (`createdAt <= endOfDay AND
 * end >= startOfDay`) so a track created at 23:59 still surfaces on
 * that day even if its first card lands a millisecond later. Stable
 * sort by `createdAt` so the dot ordering matches creation order
 * (oldest first, leftmost dot — matches how the eye scans).
 */
export function activeTracksOn(
  tracks: Track[],
  day: Date,
  nowMs: number,
): Track[] {
  const dayStart = startOfDay(day).getTime();
  const dayEnd = endOfDay(day).getTime();
  const out: Track[] = [];
  for (const w of tracks) {
    const end = w.terminalAt ?? nowMs;
    if (w.createdAt <= dayEnd && end >= dayStart) out.push(w);
  }
  out.sort((a, b) => {
    if (a.createdAt !== b.createdAt) return a.createdAt - b.createdAt;
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  });
  return out;
}

// ============================================================
// TodayPage — terminal-launcher home + calendar rail.
// Ports the design's TodayPage. The CSS class vocabulary
// (.today-page, .today-clock, …) was originally named .surf-* in the
// mockup; it was renamed to .today-* to align with the Today landing
// page identity. We keep the class names in CSS since they're stable.
// ============================================================

export function TodayPage({
  tracks,
  areas,
  onGo,
  todayTerminalId,
  todayError,
  onResetTodayTerminal,
  nowMs,
}: {
  tracks: Track[];
  areas: Area[];
  onGo: (r: Route) => void;
  /** When defined, the home panel hosts a live `<XtermView>` for this id. */
  todayTerminalId?: string | null;
  todayError?: Error | null;
  onResetTodayTerminal?: () => void;
  /**
   * Issue #250 PR 5 — pinned "now" for tests. Production leaves this
   * undefined and the calendar derives `today0` from `new Date()` and
   * its active-track predicate from `Date.now()`. Tests pin both so
   * the assertions don't drift across midnight or DST boundaries.
   */
  nowMs?: number;
}) {
  const today0 = useMemo(() => {
    const t = nowMs !== undefined ? new Date(nowMs) : new Date();
    t.setHours(0, 0, 0, 0);
    return t;
  }, [nowMs]);

  // No scheduling plugin yet — the calendar renders its shell with an
  // empty list of hour-bucketed events. When a plugin defines a
  // `scheduled` overlay this is the single line that should fold it
  // in; the dots / agenda surfacing live track activity (issue #250 PR
  // 5) runs in parallel and doesn't need the scheduling layer.
  const events = useMemo<CalEvent[]>(() => [], []);

  return (
    <div className="today-page">
      <TodayClock tracks={tracks} />
      <div className="today-grid">
        <section className="today-main">
          <TodayTerminalPanel
            terminalId={todayTerminalId ?? null}
            error={todayError ?? null}
            onReset={onResetTodayTerminal}
          />
        </section>
        <aside className="today-rail" aria-label="Calendar">
          <CalendarCard
            today0={today0}
            events={events}
            areas={areas}
            tracks={tracks}
            onGo={onGo}
            nowMs={nowMs}
          />
        </aside>
      </div>
    </div>
  );
}

/**
 * Issue #250 PR 5 — agenda row shared by the live track-activity branch
 * and the (future) hour-scheduled `CalEvent` branch. Two visual variants
 * driven off whether `hourLabel` is supplied:
 *
 *   * **scheduled event** (`hourLabel` defined) — keeps the design's
 *     `40px / 3px / 1fr` grid with the time gutter on the left. Single-
 *     line body: title + dot flag(s) for waiting/running.
 *   * **track** (`hourLabel` undefined) — drops the time gutter (tracks
 *     are day-level, not hour-bucketed; the empty 40px column made the
 *     area bar look stranded). Grid collapses to `3px / 1fr` and the
 *     body becomes a two-line column: title on top, human-readable
 *     `lifecycleLabel` underneath. Both lines clamp to a single line
 *     with ellipsis so a long title / status string can't reflow the
 *     rail.
 *
 * The right-edge 6×6 dot flag survives both variants — it's redundant
 * with the lifecycle text but lets a colorblind / fast-scanning eye
 * spot blocked / running rows without parsing the label. The full
 * lifecycle phrase is still folded into `aria-label` so screen readers
 * and axe checks don't lose it.
 */
function CalEventRow({
  track,
  hourLabel,
  areas,
  onGo,
}: {
  track: Track;
  /** Pre-formatted hour string (`"3pm"`) for scheduled events; omit
   *  for live track activity which isn't hour-bucketed. The presence /
   *  absence of this prop also selects the `cal-event--track` layout
   *  modifier (no hour gutter, lifecycle text below the title). */
  hourLabel?: string;
  areas: Area[];
  onGo: (r: Route) => void;
}) {
  const c = areaOf(track.areaId, areas);
  const isWaiting = trackNeedsUserAttention(track);
  const eventRunning = isRunning(track.lifecycle);
  const isTrack = hourLabel === undefined;
  // Reuse the canonical lifecycle phrase so the calendar agrees with
  // <TrackLifecycleBadge> / Area buckets — no parallel mapping table.
  const lifecycleText = lifecycleLabel(track.lifecycle);
  // The dot flags are visual; the full lifecycle state goes into the
  // button's aria-label so screen readers and axe checks see the same
  // information whether or not the lifecycle text line is shown.
  const displayTitle = trackDisplayTitle(track.title);
  const stateBits: string[] = [];
  if (isWaiting) stateBits.push('waiting on you');
  if (eventRunning) stateBits.push('running');
  const areaName = c?.name ?? 'Unknown area';
  const label =
    `Track ${displayTitle}` +
    (stateBits.length > 0 ? `, ${stateBits.join(', ')}` : '') +
    `, ${lifecycleText}` +
    `, in area ${areaName}`;
  return (
    <button
      className={
        'cal-event' +
        (isTrack ? ' cal-event--track' : '') +
        (isWaiting ? ' waiting' : '') +
        (eventRunning ? ' running' : '')
      }
      aria-label={label}
      onClick={() => onGo({ name: 'track', id: track.id })}
    >
      {!isTrack && (
        <span className="cal-event-time num" aria-hidden="true">
          {hourLabel}
        </span>
      )}
      <span
        className="cal-event-bar"
        style={{ background: c?.color }}
        aria-hidden="true"
      />
      <span className="cal-event-body">
        <span className="cal-event-title-row">
          <span className="cal-event-title">{displayTitle}</span>
          {isWaiting && (
            <span className="cal-event-flag warn" aria-hidden="true" />
          )}
          {eventRunning && (
            <span className="cal-event-flag run" aria-hidden="true" />
          )}
        </span>
        {isTrack && (
          <span
            className={
              'cal-event-lifecycle' +
              (isWaiting ? ' is-attention' : '')
            }
          >
            {lifecycleText}
          </span>
        )}
      </span>
    </button>
  );
}

// ---------------- TodayTerminalPanel — the real default PTY on Today ----------------
//
// Replaces the original mockup's static `SurfTerminal` (later renamed
// `TodayTerminal` in the class-name cleanup pass) with an actual live
// shell. The terminal binds to a single per-browser card hosted inside
// the kernel-owned system area + "Today" track (resolved by
// `useTodayTerminal` upstream and passed in as `terminalId`). Issue
// #175 hides the system area from the sidebar; the user only ever
// interacts with the terminal here. While the resolver runs we show a
// calm "Booting…" line.
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
    <div
      className="today-term"
      data-wheel-card
    >
      <CardHead
        className="today-term-head"
        title="~ / neige · today"
        status={
          onReset ? (
            <button
              className="today-term-host"
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
          ) : undefined
        }
      />
      <div className="today-term-body" style={{ padding: 0 }}>
        {error ? (
          <div className="today-term-line" style={{ padding: 16, color: 'var(--warn, #c00)' }}>
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
          <div className="today-term-line dim" style={{ padding: 16 }}>
            booting today's terminal…
          </div>
        )}
      </div>
    </div>
  );
}

function LiveTerminalSlot({ terminalId }: { terminalId: string }) {
  const { resolved: theme } = useTheme();
  const [, setXtermRef] = useXtermWheelTargetRef();
  return (
    <div style={{ minHeight: 360 }}>
      <Suspense fallback={<div className="synth">Loading terminal…</div>}>
        <XtermView ref={setXtermRef} terminalId={terminalId} theme={theme} />
      </Suspense>
    </div>
  );
}

// ---------------- Clock ----------------

function TodayClock({ tracks }: { tracks: Track[] }) {
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

  const runningCount = tracks.filter((w) => isRunning(w.lifecycle)).length;
  // Issue #254 — same OR'd predicate as the Sidebar's "Waiting on you"
  // section so the two surfaces agree on what counts.
  const waitingCount = tracks.filter((w) => trackNeedsUserAttention(w)).length;

  return (
    <header className="today-clock">
      <div className="today-clock-time">
        <span className="today-clock-h num">{h12}</span>
        <span className="today-clock-colon">:</span>
        <span className="today-clock-m num">{mm}</span>
        <span className="today-clock-ap">{period}</span>
      </div>
      <div className="today-clock-day">{weekday}</div>
      <div className="today-clock-status">
        <span className="today-stat run">
          <span className="today-stat-dot run" />
          <span className="today-stat-n num">{runningCount}</span>
          <span className="today-stat-lbl">running</span>
        </span>
        <span className="today-stat-sep">·</span>
        <span className="today-stat warn">
          <span className="today-stat-dot warn" />
          <span className="today-stat-n num">{waitingCount}</span>
          <span className="today-stat-lbl">waiting</span>
        </span>
      </div>
    </header>
  );
}

// ---------------- Calendar ----------------

function CalendarCard({
  today0,
  events,
  areas,
  tracks,
  onGo,
  nowMs,
}: {
  today0: Date;
  events: CalEvent[];
  areas: Area[];
  tracks: Track[];
  onGo: (r: Route) => void;
  /** Tests pin this so the "active on day" predicate doesn't drift
   *  during assertions. Defaults to live `Date.now()` in production. */
  nowMs?: number;
}) {
  const [view, setView] = useState<'week' | 'month'>('week');
  const [selected, setSelected] = useState<Date>(today0);
  const [monthCursor, setMonthCursor] = useState<Date>(() => startOfMonth(today0));
  const now = nowMs ?? Date.now();

  const eventAgenda = events
    .filter((e) => sameDay(e.date, selected))
    .sort((a, b) => a.h - b.h);
  // Issue #250 PR 5 — live track activity on the selected day. Independent
  // of `events` (which stays the future scheduling-plugin slot); both
  // lists co-exist in the agenda so a day with scheduled work *and* an
  // open track shows both rather than letting the schedule layer
  // monopolise the surface.
  const trackAgenda = useMemo(
    () => activeTracksOn(tracks, selected, now),
    [tracks, selected, now],
  );

  const selLabel = sameDay(selected, today0)
    ? 'Today'
    : selected.toLocaleDateString('en-US', {
        weekday: 'long', month: 'short', day: 'numeric',
      });

  return (
    <section className="today-card cal">
      <div className="cal-toggle-row">
        <div className="cal-toggle">
          <button
            className={view === 'week' ? 'on' : ''}
            aria-pressed={view === 'week'}
            onClick={() => setView('week')}
          >
            Week
          </button>
          <button
            className={view === 'month' ? 'on' : ''}
            aria-pressed={view === 'month'}
            onClick={() => setView('month')}
          >
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
          tracks={tracks}
          areas={areas}
          nowMs={now}
        />
      ) : (
        <CalMonth
          today0={today0}
          selected={selected}
          setSelected={setSelected}
          monthCursor={monthCursor}
          setMonthCursor={setMonthCursor}
          events={events}
          tracks={tracks}
          areas={areas}
          nowMs={now}
        />
      )}

      <div className="cal-agenda-head">{selLabel}</div>
      <div className="cal-agenda">
        {eventAgenda.length === 0 && trackAgenda.length === 0 && (
          <div className="cal-empty">Nothing scheduled.</div>
        )}
        {eventAgenda.map((e, i) => (
          <CalEventRow
            key={`evt-${i}`}
            track={e.track}
            hourLabel={fmtHour(e.h)}
            areas={areas}
            onGo={onGo}
          />
        ))}
        {trackAgenda.map((w) => (
          <CalEventRow key={`track-${w.id}`} track={w} areas={areas} onGo={onGo} />
        ))}
      </div>
    </section>
  );
}

function CalWeek({
  today0,
  selected,
  setSelected,
  events,
  tracks,
  areas,
  nowMs,
}: {
  today0: Date;
  selected: Date;
  setSelected: (d: Date) => void;
  events: CalEvent[];
  tracks: Track[];
  areas: Area[];
  nowMs: number;
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
          // Two dot sources: hour-scheduled events (future scheduling
          // plugin) and active track activity (issue #250 PR 5). De-dup
          // by `track.id` so a track with both a scheduled event and an
          // overlapping activity window contributes one dot, not two.
          const evs = events.filter((e) => sameDay(e.date, d));
          const activeIds = new Set<string>();
          const activeColors: { id: string; color: string | undefined }[] = [];
          for (const e of evs) {
            if (activeIds.has(e.track.id)) continue;
            activeIds.add(e.track.id);
            activeColors.push({ id: e.track.id, color: areaOf(e.track.areaId, areas)?.color });
          }
          for (const w of activeTracksOn(tracks, d, nowMs)) {
            if (activeIds.has(w.id)) continue;
            activeIds.add(w.id);
            activeColors.push({ id: w.id, color: areaOf(w.areaId, areas)?.color });
          }
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
                {activeColors.slice(0, 4).map((dot) => (
                  <span
                    key={dot.id}
                    className="cal-week-dot"
                    style={{ background: dot.color }}
                  />
                ))}
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
  tracks,
  areas,
  nowMs,
}: {
  today0: Date;
  selected: Date;
  setSelected: (d: Date) => void;
  monthCursor: Date;
  setMonthCursor: (d: Date) => void;
  events: CalEvent[];
  tracks: Track[];
  areas: Area[];
  nowMs: number;
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
          // Merge hour-scheduled events with active track activity (issue
          // #250 PR 5); de-dup by track id and cap to 3 dots so the cell
          // doesn't overflow.
          const evs = events.filter((e) => sameDay(e.date, d));
          const seenIds = new Set<string>();
          const dotColors: { id: string; color: string | undefined }[] = [];
          for (const e of evs) {
            if (seenIds.has(e.track.id)) continue;
            seenIds.add(e.track.id);
            dotColors.push({ id: e.track.id, color: areaOf(e.track.areaId, areas)?.color });
          }
          for (const w of activeTracksOn(tracks, d, nowMs)) {
            if (seenIds.has(w.id)) continue;
            seenIds.add(w.id);
            dotColors.push({ id: w.id, color: areaOf(w.areaId, areas)?.color });
          }
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
              {dotColors.length > 0 && (
                <span className="cal-md-dots">
                  {dotColors.slice(0, 3).map((dot) => (
                    <i key={dot.id} style={{ background: dot.color }} />
                  ))}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
