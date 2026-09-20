// The `chart.candles` block, drawn as SVG so it paints from the app's tokens (a canvas cannot read
// `oklch()` custom properties reliably). CN polarity: up is red and hollow, down is green and solid.

import { useState } from '../../../ui/state/public.ts';
import type { ChartCandlesPayload } from '../../../../../core/domain/report.ts';
import { CandlesFigure, formatPrice } from './figure.tsx';
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

const DAY_MS = 86_400_000;

export function ReportCandlesBlock({ payload }: { payload: ChartCandlesPayload }) {
  const [rangeKey, setRangeKey] = useState<RangeKey>('All');

  const all = payload.candles;
  const lastTs = all[all.length - 1]?.[0] ?? 0;
  const days = rangeDays(rangeKey);
  const visible: readonly Candle[] = days === null
    ? all
    : all.filter((candle) => candle[0] >= lastTs - days * DAY_MS);
  // Two candles is the payload's own floor; a range that falls under it falls back to the full series.
  const candles = visible.length >= 2 ? visible : all;

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
        {payload.period != null && <span className={styles.period}>{PERIOD_LABELS[payload.period]}</span>}
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

      <CandlesFigure candles={candles} overlays={payload.overlays ?? []} label={payload.symbol} />

      {payload.caption != null && payload.caption !== '' && (
        <p className={styles.caption}>{payload.caption}</p>
      )}
    </figure>
  );
}
