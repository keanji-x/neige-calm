// The `chart.series` block (#1628 S4).
//
// A series block names its data — a plugin tool and up to eight asset ids —
// and the kernel resolves the points on the read path. This block draws what
// the kernel resolved: `line`, `normalized` (every series rebased to 100 at
// its first point), `bar` and `candles` (the same drawing `chart.candles`
// uses, `../candles/figure.tsx`). It fetches nothing itself. The data comes
// through `resolve`, injected by `app/router` the way a live table's overlay
// resolver is: `features/**` must not import `app/**`, and the query — its
// key, its poll, its 409-is-a-wait rule — is the app's.
//
// SVG, for the reasons the candles block gives: tokens work (`currentColor`,
// `var(--…)`), no dependency and no lazy chunk, and the lines are markup. Up
// to eight series are told apart by hue, and the eight hues are the app's
// own identity ring (`--area-1` … `--area-8`): the only eight-step,
// theme-aware, pairwise-distinct palette the tokens offer, and "which line
// is which" is an identity question — the job that ring does for areas — not
// a state one. No literal colour appears here or in the stylesheet.
//
// What the figure says about itself — `range`, `period`, `as_of`, the
// currencies, whether a frozen block is pinned — is the kernel's own account
// of the row, printed so a number in the prose can be checked against the
// data it was written from.

import type { ChartSeriesPayload } from '../../../../../core/domain/report.ts';
import {
  isOkSeriesEntry, minCompleteThrough, rebaseToFirst, seriesCurrencies,
  type OkResolvedSeries, type OkSeriesEntry, type SeriesEntry, type SeriesPoint, type SeriesResolution,
} from '../../../../../core/domain/report-series.ts';
import {
  CandlesFigure, PAD_X, PRICE_H, VIEW_W, formatDate, movingAverage, type CandleRow,
} from '../candles/figure.tsx';
import styles from './series.module.css';

/** The identity class for a series index: `.s1` … `.s8` in the stylesheet. */
function seriesClass(index: number): string {
  return styles[`s${index + 1}`] ?? '';
}

/** The drawing box for lines and bars: the candles' price pane, no volume strip. */
const LINE_H = PRICE_H;
const PAD_Y = 6;

/** `ma20` / `ma60`: the window each name means. */
const MA_WINDOWS = Object.freeze({ ma20: 20, ma60: 60 } as const);

export function ReportSeriesBlock({ payload, blockId, rev, resolve }: {
  payload: ChartSeriesPayload;
  blockId: string;
  rev: number;
  /**
   * The app's query for this block, read by `(blockId, rev)`. Absent ⇒ the
   * surface does not carry resolved data (Today, the file viewer) and the
   * block says so instead of drawing.
   */
  resolve?: (blockId: string, rev: number) => SeriesResolution | undefined;
}) {
  const caption = payload.caption;
  if (resolve === undefined) {
    return <SeriesNotice caption={caption}
      text="This chart is resolved by the kernel and this view does not carry its data." />;
  }
  const resolution = resolve(blockId, rev);
  if (resolution === undefined || resolution.status === 'loading') {
    return <SeriesNotice caption={caption} text="Loading …" />;
  }
  switch (resolution.status) {
    case 'stale-rev':
      return <SeriesNotice caption={caption} text="Waiting for the report to refresh." />;
    case 'error':
      return <SeriesNotice caption={caption} text={`Could not load this chart: ${resolution.message}`} />;
    case 'pending':
      return <SeriesNotice caption={caption} text={
        resolution.reason != null && resolution.reason !== ''
          ? `Pending — ${resolution.reason}`
          : 'Pending — the kernel is fetching this data.'
      } />;
    case 'unavailable':
      return <SeriesNotice caption={caption} text={`Unavailable — ${resolution.reason}`} />;
    case 'ok':
      return <SeriesFigure payload={payload} resolved={resolution} />;
  }
}

/**
 * The same shape a live table uses while it waits (`LiveTableNotice`): the
 * caption plus one line of prose, never nothing. A chart that silently
 * disappears is indistinguishable from a report that never had one.
 */
function SeriesNotice({ caption, text }: { caption?: string | null; text: string }) {
  return (
    <div className={styles.wrap}>
      {caption != null && caption !== '' && <p className={styles.caption}>{caption}</p>}
      <p className={styles.caption} role="note">{text}</p>
    </div>
  );
}

function SeriesFigure({ payload, resolved }: { payload: ChartSeriesPayload; resolved: OkResolvedSeries }) {
  const frozen = payload.as_of != null;
  const currencies = seriesCurrencies(resolved.series);
  const through = minCompleteThrough(resolved.series);
  const overlays = payload.overlays ?? [];
  return (
    <figure className={styles.figure}>
      <figcaption className={styles.head}>
        <span className={styles.assets}>{resolved.series.map((entry) => entry.asset).join(' · ')}</span>
        <span className={styles.meta}>{resolved.range} {resolved.period} {resolved.view}</span>
        <span className={styles.meta}>{resolved.field}</span>
        {currencies.length > 0 && <span className={styles.meta}>{currencies.join(' / ')}</span>}
        <span className={styles.meta}>as of {resolved.as_of}</span>
        {frozen
          ? (resolved.pinned
            ? <span className={styles.pinned}>pinned</span>
            : <span className={styles.unpinned}>
                {`not pinned — source data through ${through ?? 'unknown'}`}
              </span>)
          : <span className={styles.meta}>live · resolved {resolved.resolved_at}</span>}
      </figcaption>

      {resolved.view === 'candles'
        ? <CandlesView series={resolved.series} overlays={overlays} />
        : resolved.view === 'bar'
          ? <BarsFigure series={resolved.series} />
          : <LinesFigure series={resolved.series} normalized={resolved.view === 'normalized'}
              overlays={resolved.view === 'line' ? overlays : []} />}

      {payload.caption != null && payload.caption !== '' && (
        <p className={styles.caption}>{payload.caption}</p>
      )}
    </figure>
  );
}

/* ── Lines ───────────────────────────────────────────────────────────── */

type DrawnLine = Readonly<{
  index: number;
  entry: OkSeriesEntry;
  values: readonly (readonly [number, number])[];
  /** `normalized`: the last rebased value, for the legend's "100 → x". */
  lastRebased: number | null;
}>;

type LegendRow =
  | Readonly<{ kind: 'drawn'; line: DrawnLine }>
  | Readonly<{ kind: 'unnormalizable'; index: number; entry: OkSeriesEntry }>
  | Readonly<{ kind: 'no-points'; index: number; entry: OkSeriesEntry }>
  | Readonly<{ kind: 'failed'; index: number; entry: SeriesEntry }>;

function classify(series: readonly SeriesEntry[], normalized: boolean): LegendRow[] {
  return series.map((entry, index): LegendRow => {
    if (!isOkSeriesEntry(entry)) return { kind: 'failed', index, entry };
    if (entry.points === undefined || entry.points.length === 0) return { kind: 'no-points', index, entry };
    if (!normalized) {
      const values = entry.points.map((point) => [point[0] ?? 0, point[1] ?? 0] as const);
      return { kind: 'drawn', line: { index, entry, values, lastRebased: null } };
    }
    const rebased = rebaseToFirst(entry.points);
    if (!rebased.ok) return { kind: 'unnormalizable', index, entry };
    const last = rebased.points[rebased.points.length - 1];
    return { kind: 'drawn', line: { index, entry, values: rebased.points, lastRebased: last?.[1] ?? null } };
  });
}

function extent(values: readonly number[], fallback: readonly [number, number]): readonly [number, number] {
  if (values.length === 0) return fallback;
  const low = Math.min(...values);
  const high = Math.max(...values);
  return high > low ? [low, high] : [low - 1, high + 1];
}

function LinesFigure({ series, normalized, overlays }: {
  series: readonly SeriesEntry[];
  normalized: boolean;
  overlays: readonly ('ma20' | 'ma60')[];
}) {
  const rows = classify(series, normalized);
  const lines = rows.flatMap((row) => (row.kind === 'drawn' ? [row.line] : []));
  const [tsMin, tsMax] = extent(lines.flatMap((line) => line.values.map((point) => point[0])), [0, 1]);
  const [valueMin, valueMax] = extent(lines.flatMap((line) => line.values.map((point) => point[1])), [0, 1]);
  const x = (ts: number) => PAD_X + ((ts - tsMin) / (tsMax - tsMin)) * (VIEW_W - PAD_X * 2);
  const y = (value: number) => PAD_Y + ((valueMax - value) / (valueMax - valueMin)) * (LINE_H - PAD_Y * 2);
  const toPoints = (values: readonly (readonly [number, number])[]) =>
    values.map((point) => `${x(point[0])},${y(point[1])}`).join(' ');

  const description = lines.length === 0
    ? 'No series could be drawn.'
    : `${lines.length} series from ${formatDate(tsMin)} to ${formatDate(tsMax)}`
      + (normalized ? ', each rebased to 100 at its first point.' : `, ${valueMin.toFixed(2)} to ${valueMax.toFixed(2)}.`);

  return (
    <>
      <svg
        className={styles.svg}
        viewBox={`0 0 ${VIEW_W} ${LINE_H}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={description}
      >
        <g aria-hidden="true">
          {normalized && lines.length > 0 && (
            <line className={styles.baseline} x1={PAD_X} x2={VIEW_W - PAD_X} y1={y(100)} y2={y(100)}
              vectorEffect="non-scaling-stroke" />
          )}
          {lines.map((line) => (
            <polyline
              key={line.entry.asset}
              className={`${styles.line} ${seriesClass(line.index)}`}
              data-nc-series={line.entry.asset}
              points={toPoints(line.values)}
              vectorEffect="non-scaling-stroke"
            />
          ))}
          {lines.flatMap((line) => overlays.map((overlay) => {
            const averaged = movingAverage(line.values.map((point) => point[1]), MA_WINDOWS[overlay]);
            const points = line.values
              .map((point, index) => {
                const value = averaged[index];
                return value == null ? null : `${x(point[0])},${y(value)}`;
              })
              .filter((point): point is string => point !== null)
              .join(' ');
            return (
              <polyline
                key={`${line.entry.asset}:${overlay}`}
                className={`${styles.overlay} ${seriesClass(line.index)}`}
                points={points}
                vectorEffect="non-scaling-stroke"
              />
            );
          }))}
        </g>
      </svg>
      <Legend rows={rows} overlays={overlays} />
    </>
  );
}

/* ── Bars ────────────────────────────────────────────────────────────── */

function BarsFigure({ series }: { series: readonly SeriesEntry[] }) {
  const rows = classify(series, false);
  const lines = rows.flatMap((row) => (row.kind === 'drawn' ? [row.line] : []));
  // One slot per distinct timestamp, in time order; a series missing a bar
  // there (a holiday on one venue) simply leaves its slot empty.
  const stamps = [...new Set(lines.flatMap((line) => line.values.map((point) => point[0])))].sort((a, b) => a - b);
  const values = lines.flatMap((line) => line.values.map((point) => point[1]));
  // Bars grow from zero: zero is a value, not a gap, and a bar that started at
  // the minimum would draw the smallest value as nothing.
  const valueMin = Math.min(0, ...values);
  const valueMax = Math.max(0, ...values);
  const span = valueMax > valueMin ? valueMax - valueMin : 1;
  const y = (value: number) => PAD_Y + ((valueMax - value) / span) * (LINE_H - PAD_Y * 2);
  const slot = (VIEW_W - PAD_X * 2) / Math.max(1, stamps.length);
  const gap = slot * 0.2;
  const width = Math.max(0.5, (slot - gap) / Math.max(1, lines.length));
  const slotIndex = new Map(stamps.map((ts, index) => [ts, index]));

  const description = lines.length === 0
    ? 'No series could be drawn.'
    : `${lines.length} series as bars over ${stamps.length} periods from ${formatDate(stamps[0] ?? 0)}`
      + ` to ${formatDate(stamps[stamps.length - 1] ?? 0)}, ${valueMin.toFixed(2)} to ${valueMax.toFixed(2)}.`;

  return (
    <>
      <svg
        className={styles.svg}
        viewBox={`0 0 ${VIEW_W} ${LINE_H}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={description}
      >
        <g aria-hidden="true">
          {lines.map((line, position) => line.values.map((point) => {
            const index = slotIndex.get(point[0]) ?? 0;
            const left = PAD_X + index * slot + gap / 2 + position * width;
            const top = y(Math.max(0, point[1]));
            const bottom = y(Math.min(0, point[1]));
            return (
              <rect
                key={`${line.entry.asset}:${point[0]}`}
                className={`${styles.bar} ${seriesClass(line.index)}`}
                data-nc-series={line.entry.asset}
                x={left}
                y={top}
                width={width}
                height={Math.max(0.5, bottom - top)}
              />
            );
          }))}
        </g>
      </svg>
      <Legend rows={rows} overlays={[]} />
    </>
  );
}

/* ── Candles ─────────────────────────────────────────────────────────── */

/** `[ts, open, high, low, close, volume?]` rows, or `null` if any row is shorter. */
function toCandleRows(points: readonly SeriesPoint[]): CandleRow[] | null {
  const rows: CandleRow[] = [];
  for (const point of points) {
    const [ts, open, high, low, close, volume] = point;
    if (ts === undefined || open === undefined || high === undefined || low === undefined || close === undefined) {
      return null;
    }
    rows.push([ts, open, high, low, close, volume ?? null]);
  }
  return rows;
}

function CandlesView({ series, overlays }: {
  series: readonly SeriesEntry[];
  overlays: readonly ('ma20' | 'ma60')[];
}) {
  // The contract admits exactly one series for `candles`; anything else in
  // the array is reported through the legend, not guessed at.
  const entry = series[0];
  if (entry === undefined || !isOkSeriesEntry(entry)) {
    return <Legend rows={classify(series, false)} overlays={[]} />;
  }
  const candles = toCandleRows(entry.points ?? []);
  if (candles === null || candles.length < 2) {
    return <Legend rows={[{ kind: 'no-points', index: 0, entry }]} overlays={[]} />;
  }
  return (
    <>
      <CandlesFigure candles={candles} overlays={overlays} label={entry.asset} />
      <Legend rows={classify(series, false)} overlays={[]} />
    </>
  );
}

/* ── Legend ──────────────────────────────────────────────────────────── */

function signed(value: number | null): string {
  if (value === null) return 'n/a';
  return `${value >= 0 ? '+' : ''}${value.toFixed(2)}%`;
}

function Legend({ rows, overlays }: { rows: readonly LegendRow[]; overlays: readonly ('ma20' | 'ma60')[] }) {
  return (
    <ul className={styles.legend} aria-label="Series">
      {rows.map((row) => {
        const index = row.kind === 'drawn' ? row.line.index : row.index;
        const asset = row.kind === 'drawn' ? row.line.entry.asset : row.entry.asset;
        const swatch = <span className={`${styles.swatch} ${seriesClass(index)}`} aria-hidden="true" />;
        switch (row.kind) {
          case 'drawn': {
            const { entry, lastRebased } = row.line;
            return (
              <li key={asset} className={styles.legendItem}>
                {swatch}
                <span className={styles.legendAsset}>{asset}</span>
                {entry.currency != null && entry.currency !== '' && <span className={styles.legendMeta}>{entry.currency}</span>}
                <span className={styles.legendMeta}>{signed(entry.change_pct)}</span>
                {lastRebased !== null && (
                  <span className={styles.legendMeta}>{`100 → ${lastRebased.toFixed(2)}`}</span>
                )}
                <span className={styles.legendMeta}>{`${entry.first[0]} – ${entry.last[0]}`}</span>
              </li>
            );
          }
          case 'unnormalizable':
            return (
              <li key={asset} className={styles.legendItem}>
                {swatch}
                <span className={styles.legendAsset}>{asset}</span>
                <span className={styles.legendMeta}>cannot normalize (first value ≤ 0)</span>
              </li>
            );
          case 'no-points':
            return (
              <li key={asset} className={styles.legendItem}>
                {swatch}
                <span className={styles.legendAsset}>{asset}</span>
                <span className={styles.legendMeta}>no points in this read</span>
              </li>
            );
          case 'failed':
            return (
              <li key={asset} className={styles.legendItem}>
                {swatch}
                <span className={styles.legendAsset}>{asset}</span>
                <span className={styles.legendMeta}>
                  {row.entry.status}{'reason' in row.entry && row.entry.reason != null && row.entry.reason !== '' ? ` — ${row.entry.reason}` : ''}
                </span>
              </li>
            );
        }
      })}
      {overlays.map((overlay) => (
        <li key={overlay} className={styles.legendItem}>
          <span className={`${styles.swatch} ${styles.swatchOverlay}`} aria-hidden="true" />
          <span className={styles.legendMeta}>{overlay.toUpperCase()}</span>
        </li>
      ))}
    </ul>
  );
}
