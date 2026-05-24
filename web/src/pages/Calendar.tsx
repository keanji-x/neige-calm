// CalendarPage — issue #250 PR 5.
//
// Top-level `/calendar` route. Aggregates every wave the user can see
// across every cove and lays them out on a Mon–Sun weekly grid; each
// wave is a single horizontal bar that starts on its `created_at` day
// and runs to `terminal_at ?? now()`, clipped to the visible window.
// The bar colour is the owning cove's `color` so the user can tell
// projects apart at a glance.
//
// MVP scope (issue Non-goals):
//   * Week-only (no month).
//   * Day-resolution (no hour grid yet).
//   * No cove-filter toggle — every wave the API returns is rendered.
//   * No empty-click create — that's PR 6.
//
// Lifecycle is encoded with border style + opacity (see `lifecycleStyle`
// below) rather than colour so cove identity stays the primary visual
// signal. The exact treatment is intentionally restrained — calm is a
// design value, not just a name.

import { useMemo } from 'react';
import { useState } from '../shared/state';
import type { Cove, Route, WaveLifecycle } from '../types';

const DAY_NAMES = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

const ONE_DAY_MS = 24 * 60 * 60 * 1000;

/**
 * The Monday 00:00 (local time) that owns `d`. Mirrors `startOfWeek` in
 * `Today.tsx` — JS `getDay()` returns 0..6 with Sunday=0, so the
 * `(getDay() + 6) % 7` trick rotates Monday to 0.
 */
export function startOfWeek(d: Date): Date {
  const r = new Date(d);
  const dow = (r.getDay() + 6) % 7;
  r.setDate(r.getDate() - dow);
  r.setHours(0, 0, 0, 0);
  return r;
}

/**
 * The local-time start of the day that owns `ts` (unix ms).
 */
export function startOfLocalDay(ts: number): Date {
  const r = new Date(ts);
  r.setHours(0, 0, 0, 0);
  return r;
}

/**
 * 0-based day index of `ts` relative to `weekStart`. Used to compute the
 * `grid-column` start/end for a wave's bar. NOT clamped — callers handle
 * <0 / >6 themselves to fold partially-visible bars into the visible
 * window.
 */
export function dayIndex(ts: number, weekStart: Date): number {
  // Whole-day floor so DST transitions (one 23h / one 25h day per year)
  // don't bump the index by ±1. Strategy: compare the
  // YMD of the timestamp's local day against the YMD of weekStart,
  // counted in calendar days. `Math.round` shrugs off the ±1h drift from
  // DST.
  const a = startOfLocalDay(ts).getTime();
  return Math.round((a - weekStart.getTime()) / ONE_DAY_MS);
}

/**
 * For a wave, the inclusive [start, end] column indices it visually
 * occupies inside the week starting at `weekStart`. Returns `null` when
 * the wave doesn't overlap the visible window at all.
 *
 * `terminal_at == null` means the wave is still open; we extend it to
 * "right now" so the bar grows live as time passes.
 *
 * Bars that start before the window are clipped to column 0 and given a
 * subtle "trails off the left edge" treatment by the renderer
 * (`clipLeft`); same on the right with `clipRight`.
 */
export interface WaveSpan {
  /** 0-based; clipped to [0, 6]. */
  start: number;
  /** 0-based inclusive; clipped to [0, 6]. */
  end: number;
  /** True when the wave's real start is to the left of the window. */
  clipLeft: boolean;
  /** True when the wave is still open OR ends past the right edge. */
  clipRight: boolean;
  /** Source created_at, for tooltip / hover detail. */
  createdAt: number;
  /** Resolved terminal_at (real value or `now()` fallback). */
  endAt: number;
}

export function waveSpanInWeek(
  createdAt: number,
  terminalAt: number | null,
  weekStart: Date,
  now: number,
): WaveSpan | null {
  const weekEnd = new Date(weekStart);
  weekEnd.setDate(weekEnd.getDate() + 6);
  weekEnd.setHours(23, 59, 59, 999);

  const endAt = terminalAt ?? now;
  // Hard miss: wave terminated before the week or started after it.
  if (endAt < weekStart.getTime()) return null;
  if (createdAt > weekEnd.getTime()) return null;

  const rawStart = dayIndex(createdAt, weekStart);
  const rawEnd = dayIndex(endAt, weekStart);
  const start = Math.max(0, Math.min(6, rawStart));
  const end = Math.max(start, Math.min(6, rawEnd));
  return {
    start,
    end,
    clipLeft: rawStart < 0,
    clipRight: rawEnd > 6 || terminalAt === null,
    createdAt,
    endAt,
  };
}

/**
 * Wave shape consumed by the calendar — narrower than the kernel `Wave`
 * so the page can stay decoupled from the wire layer and tests can hand
 * in fixtures without faking unused fields. The router-side
 * `CalendarComponent` (in `router.tsx`) does the kernel → calendar
 * adaptation; this module never imports `wire.ts`.
 */
export interface CalendarWave {
  id: string;
  title: string;
  coveId: string;
  lifecycle: WaveLifecycle;
  createdAt: number;
  terminalAt: number | null;
  cwd: string;
}

/**
 * Lifecycle → visual treatment. Keep the vocabulary calm: opacity for
 * "is this still mattering?", border style for "does it want attention?".
 * The bar colour comes from the cove, NOT lifecycle — colour is the
 * primary axis the user reads.
 */
function lifecycleStyle(l: WaveLifecycle): {
  opacity: number;
  borderStyle: 'solid' | 'dashed';
  /** True when we should paint a thin red attention-grabbing border. */
  attention: boolean;
  /** Optional small glyph rendered before the title. */
  glyph?: string;
  /** Strikethrough text for canceled. */
  strike: boolean;
  /** Diagonal-stripe overlay for reviewing. */
  stripes: boolean;
} {
  switch (l) {
    case 'draft':
      return { opacity: 0.55, borderStyle: 'dashed', attention: false, strike: false, stripes: false };
    case 'planning':
    case 'dispatching':
      return { opacity: 0.8, borderStyle: 'solid', attention: false, strike: false, stripes: false };
    case 'working':
      return { opacity: 1, borderStyle: 'solid', attention: false, strike: false, stripes: false };
    case 'blocked':
      return { opacity: 1, borderStyle: 'solid', attention: true, strike: false, stripes: false };
    case 'reviewing':
      return { opacity: 1, borderStyle: 'solid', attention: false, strike: false, stripes: true };
    case 'done':
      return { opacity: 1, borderStyle: 'solid', attention: false, glyph: '✓', strike: false, stripes: false };
    case 'canceled':
      return { opacity: 0.4, borderStyle: 'solid', attention: false, strike: true, stripes: false };
    case 'failed':
      return { opacity: 1, borderStyle: 'solid', attention: true, glyph: '✗', strike: false, stripes: false };
  }
}

/**
 * Lay laned waves out into the minimum number of horizontal lanes so no
 * two overlapping spans share a lane. Classic interval-graph greedy: walk
 * waves in `createdAt` order, drop each onto the first lane whose
 * tail-end falls strictly before this wave's start.
 *
 * Exported for the unit tests; not used outside this module.
 */
export function packLanes<T extends { span: WaveSpan }>(items: T[]): T[][] {
  const lanes: T[][] = [];
  // Iteration order needs to be deterministic for both rendering and
  // tests; the caller sorts by createdAt ascending.
  for (const item of items) {
    let placed = false;
    for (const lane of lanes) {
      const tail = lane[lane.length - 1];
      if (!tail) continue;
      if (tail.span.end < item.span.start) {
        lane.push(item);
        placed = true;
        break;
      }
    }
    if (!placed) lanes.push([item]);
  }
  return lanes;
}

// ============================================================
// CalendarPage
// ============================================================

export interface CalendarPageProps {
  waves: CalendarWave[];
  coves: Cove[];
  /** Week anchor — any timestamp inside the desired week. Defaults to
   *  `Date.now()` (i.e. the current local week). Tests pass a fixed
   *  value to make assertions deterministic across runs. */
  weekAnchor?: number;
  /** Reference "now" for live bar extension when `terminal_at == null`.
   *  Tests pin this so a wave whose `terminal_at` is null doesn't grow
   *  during the assertion phase. Defaults to `Date.now()`. */
  nowMs?: number;
  /** Fired when the user clicks one of the wave bars. */
  onGo: (r: Route) => void;
  /** Fired when the user picks a new week anchor (left/right arrows or
   *  "This week"). Optional so tests can drive the inner state directly
   *  via `weekAnchor`. */
  onWeekChange?: (newAnchorMs: number) => void;
}

export function CalendarPage({
  waves,
  coves,
  weekAnchor,
  nowMs,
  onGo,
  onWeekChange,
}: CalendarPageProps) {
  // The anchor + now bracket is split because tests want to vary "what
  // week are we showing?" without also pinning "when is now?" — the two
  // concerns are independent (jumping to last month doesn't pretend it's
  // last month for the purpose of clipping an open wave).
  const initialAnchor = weekAnchor ?? Date.now();
  const [internalAnchor, setInternalAnchor] = useState(initialAnchor);
  // Controlled vs. uncontrolled: if the parent passes `weekAnchor`, that
  // wins; otherwise we own the cursor. Mirrors React's input pattern.
  const anchor = weekAnchor ?? internalAnchor;
  const now = nowMs ?? Date.now();

  const weekStart = useMemo(() => startOfWeek(new Date(anchor)), [anchor]);
  const todayStart = useMemo(() => startOfLocalDay(now), [now]);

  // Day headers — date + abbreviated weekday name.
  const days = useMemo(() => {
    const out: { date: Date; label: string; isToday: boolean }[] = [];
    for (let i = 0; i < 7; i++) {
      const d = new Date(weekStart);
      d.setDate(d.getDate() + i);
      out.push({
        date: d,
        label: `${DAY_NAMES[i]} ${d.getDate()}`,
        isToday: d.getTime() === todayStart.getTime(),
      });
    }
    return out;
  }, [weekStart, todayStart]);

  // Resolve each wave into a placed span; drop waves outside the window.
  const placed = useMemo(() => {
    type Placed = { wave: CalendarWave; span: WaveSpan; cove: Cove | undefined };
    const list: Placed[] = [];
    for (const w of waves) {
      const span = waveSpanInWeek(w.createdAt, w.terminalAt, weekStart, now);
      if (!span) continue;
      list.push({ wave: w, span, cove: coves.find((c) => c.id === w.coveId) });
    }
    list.sort((a, b) => {
      // Primary: created_at; tiebreak on id so the lane assignment is
      // stable across renders. (The kernel returns waves sorted by
      // created_at already, but we don't take a free dependency on that.)
      if (a.wave.createdAt !== b.wave.createdAt) return a.wave.createdAt - b.wave.createdAt;
      return a.wave.id < b.wave.id ? -1 : a.wave.id > b.wave.id ? 1 : 0;
    });
    return list;
  }, [waves, coves, weekStart, now]);

  const lanes = useMemo(() => packLanes(placed), [placed]);

  const goPrev = () => {
    const next = anchor - 7 * ONE_DAY_MS;
    if (weekAnchor === undefined) setInternalAnchor(next);
    onWeekChange?.(next);
  };
  const goNext = () => {
    const next = anchor + 7 * ONE_DAY_MS;
    if (weekAnchor === undefined) setInternalAnchor(next);
    onWeekChange?.(next);
  };
  const goThisWeek = () => {
    const next = Date.now();
    if (weekAnchor === undefined) setInternalAnchor(next);
    onWeekChange?.(next);
  };

  // Headline reads "May 19 – 25" for a week wholly in May; spans across
  // months read "Apr 28 – May 4". Intl.DateTimeFormat handles the locale
  // wiring with no extra deps.
  const headline = useMemo(() => {
    const last = new Date(weekStart);
    last.setDate(last.getDate() + 6);
    const fmt = new Intl.DateTimeFormat(undefined, {
      month: 'short',
      day: 'numeric',
    });
    const yearFmt = new Intl.DateTimeFormat(undefined, { year: 'numeric' });
    return `${fmt.format(weekStart)} – ${fmt.format(last)}, ${yearFmt.format(weekStart)}`;
  }, [weekStart]);

  return (
    <section className="calendar-page" aria-label="Calendar">
      <header className="calendar-head">
        <h1 className="calendar-title">{headline}</h1>
        <div className="calendar-nav" role="group" aria-label="Week navigation">
          <button
            type="button"
            className="calendar-nav-btn"
            onClick={goPrev}
            aria-label="Previous week"
          >
            ‹
          </button>
          <button
            type="button"
            className="calendar-nav-btn calendar-nav-this"
            onClick={goThisWeek}
          >
            This week
          </button>
          <button
            type="button"
            className="calendar-nav-btn"
            onClick={goNext}
            aria-label="Next week"
          >
            ›
          </button>
        </div>
      </header>

      <div
        className="calendar-grid"
        role="grid"
        aria-rowcount={lanes.length + 1}
        aria-colcount={7}
      >
        <div className="calendar-row calendar-row-head" role="row">
          {days.map((d) => (
            <div
              key={d.date.toISOString()}
              role="columnheader"
              className={'calendar-col-head' + (d.isToday ? ' is-today' : '')}
            >
              {d.label}
            </div>
          ))}
        </div>

        <div className="calendar-body">
          {/* Background day columns so the grid lines read correctly even
              when a lane has no bars in a given day. */}
          <div className="calendar-bg-cols" aria-hidden="true">
            {days.map((d) => (
              <div
                key={d.date.toISOString()}
                className={'calendar-bg-col' + (d.isToday ? ' is-today' : '')}
              />
            ))}
          </div>

          {lanes.length === 0 ? (
            <div className="calendar-empty">No waves this week.</div>
          ) : (
            lanes.map((lane, laneIdx) => (
              <div
                key={laneIdx}
                role="row"
                className="calendar-lane"
                aria-rowindex={laneIdx + 2}
              >
                {lane.map(({ wave, span, cove }) => (
                  <WaveBar
                    key={wave.id}
                    wave={wave}
                    span={span}
                    cove={cove}
                    onGo={onGo}
                  />
                ))}
              </div>
            ))
          )}
        </div>
      </div>
    </section>
  );
}

// ============================================================
// WaveBar
// ============================================================

function WaveBar({
  wave,
  span,
  cove,
  onGo,
}: {
  wave: CalendarWave;
  span: WaveSpan;
  cove: Cove | undefined;
  onGo: (r: Route) => void;
}) {
  const style = lifecycleStyle(wave.lifecycle);
  const color = cove?.color ?? 'var(--text-3)';
  const coveName = cove?.name ?? 'Unknown cove';

  // Tooltip lines (newline-joined for the native `title` attr).
  const tooltip = [
    wave.title,
    `Cove: ${coveName}`,
    `Lifecycle: ${wave.lifecycle}`,
    `cwd: ${wave.cwd || '(unset)'}`,
    `Created: ${new Date(wave.createdAt).toLocaleString()}`,
    wave.terminalAt
      ? `Ended: ${new Date(wave.terminalAt).toLocaleString()}`
      : 'Still open',
  ].join('\n');

  // `grid-column` is 1-indexed; our `start/end` are 0-indexed. The CSS
  // grid spans columns inclusively up to but not including the end-line,
  // so `gridColumnEnd = end + 2`.
  const colStart = span.start + 1;
  const colEnd = span.end + 2;

  // `attention` and `stripes` are layered onto inline style because the
  // exact accent colour depends on the cove (we don't want a single
  // `--bar-color` CSS var because that would force the dashed border /
  // stripe overlay to also use the same hue — the attention/stripe layers
  // are deliberately decoupled).
  const bg = style.stripes
    ? `repeating-linear-gradient(45deg, ${color} 0 8px, color-mix(in srgb, ${color} 60%, transparent) 8px 16px)`
    : color;

  const ariaLabel =
    `Wave ${wave.title} in cove ${coveName}, ${wave.lifecycle}` +
    (span.clipLeft ? ', continues from earlier' : '') +
    (span.clipRight ? ', ongoing' : '');

  return (
    <button
      type="button"
      className={
        'calendar-bar' +
        (style.attention ? ' is-attention' : '') +
        (span.clipLeft ? ' clip-left' : '') +
        (span.clipRight ? ' clip-right' : '')
      }
      style={{
        gridColumn: `${colStart} / ${colEnd}`,
        background: bg,
        opacity: style.opacity,
        borderStyle: style.borderStyle,
        textDecoration: style.strike ? 'line-through' : 'none',
      }}
      title={tooltip}
      aria-label={ariaLabel}
      data-wave-id={wave.id}
      data-cove-id={wave.coveId}
      data-lifecycle={wave.lifecycle}
      onClick={() => onGo({ name: 'wave', id: wave.id })}
    >
      {style.glyph && <span className="calendar-bar-glyph" aria-hidden="true">{style.glyph}</span>}
      <span className="calendar-bar-title">{wave.title}</span>
      <span className="calendar-bar-cove" aria-hidden="true">{coveName}</span>
    </button>
  );
}
