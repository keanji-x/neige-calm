// The `chart.candles` block.
//
// Drawn as **SVG**, not on a canvas and not by a charting library. That is a
// deliberate departure from the legacy web's `lightweight-charts` port, and it
// buys three things this app specifically needs:
//
//   * **The tokens work.** A canvas cannot read `oklch()` custom properties
//     reliably, which is why the legacy chart carries a duplicated hex palette
//     per theme that has to be kept in sync with the stylesheet by hand. SVG
//     paints with `currentColor` and `var(--…)`, so the chart is themed by the
//     same tokens as everything else and there is no second palette to drift.
//   * **No dependency and no lazy chunk.** The reason §8.3 asks for a lazily
//     loaded chart library is the ~45KB a report without a chart should not
//     pay. This pays none of it, so the requirement is met by having no bytes
//     rather than by deferring them.
//   * **It is markup.** The candles are elements, so they inherit the app's
//     reduced-motion and focus rules for free.
//
// Visual contract (unchanged from the legacy figure): CN polarity — up is red,
// down is green — and **up candles are hollow while down candles are solid**.
// The fill is a second encoding channel on top of hue, because red/green alone
// is not readable under the most common colour-vision deficiency (§原则 5).

import { useState } from '../../../ui/state/public.ts';
import type { ChartCandlesPayload } from '../../../../../core/domain/report.ts';
import styles from './candles.module.css';

type Candle = ChartCandlesPayload['candles'][number];

const RANGE_KEYS = Object.freeze(['1M', '3M', '6M', '1Y', 'All'] as const);

type RangeKey = (typeof RANGE_KEYS)[number];

/** `null` is "all of it", which is the only range that is not a day count. */
function rangeDays(key: RangeKey): number | null {
  switch (key) {
    case '1M': return 30;
    case '3M': return 91;
    case '6M': return 182;
    case '1Y': return 365;
    case 'All': return null;
  }
}

const PERIOD_LABELS = Object.freeze({ day: '日线', week: '周线', month: '月线' });

// The drawing box. It is a `viewBox`, so these are ratios rather than pixels:
// the figure scales to whatever width the breakout gives it.
const VIEW_W = 740;
const PRICE_H = 208;
const VOLUME_H = 40;
const GAP_H = 8;
const VIEW_H = PRICE_H + GAP_H + VOLUME_H;
const PAD_X = 4;

const DAY_MS = 86_400_000;

function movingAverage(closes: readonly number[], window: number): (number | null)[] {
  const out: (number | null)[] = new Array<number | null>(closes.length).fill(null);
  let sum = 0;
  for (let index = 0; index < closes.length; index += 1) {
    sum += closes[index] ?? 0;
    if (index >= window) sum -= closes[index - window] ?? 0;
    if (index >= window - 1) out[index] = sum / window;
  }
  return out;
}

function formatPrice(value: number): string {
  return value.toFixed(2);
}

function formatVolume(value: number): string {
  if (value >= 1e8) return `${(value / 1e8).toFixed(2)}亿`;
  if (value >= 1e4) return `${(value / 1e4).toFixed(0)}万`;
  return String(Math.round(value));
}

/** UTC, deliberately: the payload's timestamps are the kernel's, and a chart
 *  that shifted its dates by the reader's timezone would disagree with the
 *  prose next to it. */
function formatDate(tsMs: number): string {
  const date = new Date(tsMs);
  const month = String(date.getUTCMonth() + 1).padStart(2, '0');
  const day = String(date.getUTCDate()).padStart(2, '0');
  return `${date.getUTCFullYear()}-${month}-${day}`;
}

export function ReportCandlesBlock({ payload }: { payload: ChartCandlesPayload }) {
  const [rangeKey, setRangeKey] = useState<RangeKey>('All');

  const all = payload.candles;
  const lastTs = all[all.length - 1]?.[0] ?? 0;
  const days = rangeDays(rangeKey);
  const visible: readonly Candle[] = days === null
    ? all
    : all.filter((candle) => candle[0] >= lastTs - days * DAY_MS);
  // Two candles is the payload's own floor; a filtered range that falls under
  // it draws nothing meaningful, so it falls back to the full series rather
  // than to an empty box.
  const candles = visible.length >= 2 ? visible : all;

  const closes = candles.map((candle) => candle[4]);
  const highs = candles.map((candle) => candle[2]);
  const lows = candles.map((candle) => candle[3]);
  const volumes = candles.map((candle) => candle[5] ?? 0);
  const high = Math.max(...highs);
  const low = Math.min(...lows);
  const span = high - low || 1;
  const volumeMax = Math.max(...volumes) || 1;

  const step = (VIEW_W - PAD_X * 2) / candles.length;
  const bodyWidth = Math.max(1, Math.min(9, step * 0.68));
  const centerX = (index: number) => PAD_X + step * (index + 0.5);
  const priceY = (value: number) => ((high - value) / span) * PRICE_H;

  const overlays = payload.overlays ?? [];
  const overlayLines = overlays.map((overlay) => ({
    key: overlay,
    className: overlay === 'ma20' ? styles.ma20 : styles.ma60,
    points: movingAverage(closes, overlay === 'ma20' ? 20 : 60)
      .map((value, index) => (value === null ? null : `${centerX(index)},${priceY(value)}`))
      .filter((point): point is string => point !== null)
      .join(' '),
  }));

  const first = candles[0];
  const last = candles[candles.length - 1];
  const change = first !== undefined && last !== undefined && first[1] !== 0
    ? ((last[4] - first[1]) / first[1]) * 100
    : 0;
  const rising = change >= 0;

  return (
    <figure className={styles.figure}>
      <figcaption className={styles.head}>
        <span className={styles.symbol}>{payload.symbol}</span>
        {payload.period !== undefined && <span className={styles.period}>{PERIOD_LABELS[payload.period]}</span>}
        {last !== undefined && <span className={styles.last}>{formatPrice(last[4])}</span>}
        <span className={rising ? styles.changeUp : styles.changeDown}>
          {rising ? '+' : ''}{change.toFixed(2)}%
        </span>
        <span className={styles.ranges} role="group" aria-label="Chart range">
          {RANGE_KEYS.map((candidate) => (
            <button
              key={candidate}
              type="button"
              className={styles.range}
              aria-pressed={candidate === rangeKey}
              onClick={() => setRangeKey(candidate)}
            >
              {candidate}
            </button>
          ))}
        </span>
      </figcaption>

      {/*
        The chart is a figure, not a control: the whole of it is described once,
        in words, for a reader who is not going to look at 200 rectangles. The
        rectangles themselves are `aria-hidden` — announcing each candle would
        be a worse experience than announcing none.
      */}
      <svg
        className={styles.svg}
        viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={
          `${payload.symbol}: ${candles.length} candles from ${formatDate(candles[0]?.[0] ?? 0)}`
          + ` to ${formatDate(last?.[0] ?? 0)}, ${formatPrice(low)} to ${formatPrice(high)},`
          + ` ${change >= 0 ? 'up' : 'down'} ${Math.abs(change).toFixed(2)}% over the range.`
        }
      >
        <g aria-hidden="true">
          {candles.map((candle, index) => {
            const [ts, open, candleHigh, candleLow, close, volume] = candle;
            const up = close >= open;
            const x = centerX(index);
            const bodyTop = priceY(Math.max(open, close));
            const bodyBottom = priceY(Math.min(open, close));
            const volumeHeight = ((volume ?? 0) / volumeMax) * VOLUME_H;
            return (
              <g key={ts} className={up ? styles.up : styles.down}>
                <line
                  className={styles.wick}
                  x1={x} x2={x}
                  y1={priceY(candleHigh)} y2={priceY(candleLow)}
                  vectorEffect="non-scaling-stroke"
                />
                <rect
                  className={styles.body}
                  x={x - bodyWidth / 2}
                  y={bodyTop}
                  width={bodyWidth}
                  /* A doji has open === close; without a floor its body would
                     be a zero-height rect, i.e. invisible exactly where the
                     market did nothing — which is information. */
                  height={Math.max(1, bodyBottom - bodyTop)}
                  vectorEffect="non-scaling-stroke"
                />
                {volume !== undefined && (
                  <rect
                    className={styles.volume}
                    x={x - bodyWidth / 2}
                    y={PRICE_H + GAP_H + (VOLUME_H - volumeHeight)}
                    width={bodyWidth}
                    height={Math.max(0.5, volumeHeight)}
                  />
                )}
              </g>
            );
          })}
          {overlayLines.map((line) => (
            <polyline
              key={line.key}
              className={`${styles.overlay} ${line.className}`}
              points={line.points}
              vectorEffect="non-scaling-stroke"
            />
          ))}
        </g>
      </svg>

      <div className={styles.legend}>
        <span className={styles.swatchUp} aria-hidden="true" />
        <span>涨（空心）</span>
        <span className={styles.swatchDown} aria-hidden="true" />
        <span>跌（实心）</span>
        {overlays.map((overlay) => (
          <span key={overlay} className={styles.legendMa}>
            <span
              className={overlay === 'ma20' ? styles.swatchMa20 : styles.swatchMa60}
              aria-hidden="true"
            />
            {overlay.toUpperCase()}
          </span>
        ))}
        <span className={styles.legendRange}>
          {formatDate(candles[0]?.[0] ?? 0)} – {formatDate(last?.[0] ?? 0)}
          {' · '}
          {formatVolume(volumes.reduce((sum, value) => sum + value, 0))}
        </span>
      </div>

      {payload.caption !== undefined && payload.caption !== '' && (
        <p className={styles.caption}>{payload.caption}</p>
      )}
    </figure>
  );
}
