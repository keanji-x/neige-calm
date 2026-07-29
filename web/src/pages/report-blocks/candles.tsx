// `chart.candles` report block — lightweight-charts (v5 API) candlestick
// figure with client-side MA overlays and range filtering.
//
// Visual contract (from the #960 design demo):
//   * 红涨绿跌 (CN market polarity): up = red, down = green.
//   * Rising candles are HOLLOW (transparent body + colored border), falling
//     candles are solid — a second encoding channel on top of hue, because
//     the red/green pair alone is not enough for color-vision deficiency.
//   * No card frame: hairline rules above/below (CSS), fig-top header
//     (symbol + last + range change + range switcher), fig-cap legend with
//     hollow/solid swatches.
//   * Colors are hex mirrors of the calm.css --up/--down tokens — the chart
//     draws on canvas, where reading oklch custom properties is unreliable,
//     so the palette is duplicated here per theme and must be kept in sync
//     with calm.css.

import { useEffect, useMemo, useRef } from 'react';
import {
  CandlestickSeries,
  ColorType,
  createChart,
  HistogramSeries,
  LineSeries,
  LineStyle,
  type MouseEventParams,
  type UTCTimestamp,
} from 'lightweight-charts';
import { useTheme } from '../../app/theme';
import { useState } from '../../shared/state';
import type { ChartCandlesPayload } from '../../cards/builtins/wave-report';

interface ChartPalette {
  up: string;
  down: string;
  grid: string;
  axisText: string;
  ma20: string;
  ma60: string;
}

// Hex mirrors of calm.css tokens (light/dark): --up/--down exactly; grid ≈
// --hairline, axisText ≈ --text-4, ma20 ≈ --accent, ma60 ≈ --text-3.
const PALETTES: Record<'light' | 'dark', ChartPalette> = {
  light: {
    up: '#b3271c',
    down: '#2ba471',
    grid: '#e6e8ed',
    axisText: '#b0b4bf',
    ma20: '#3a6ecf',
    ma60: '#6d7280',
  },
  dark: {
    up: '#e94f42',
    down: '#34a87e',
    grid: '#33363f',
    axisText: '#6d7280',
    ma20: '#7aa0e8',
    ma60: '#8f94a3',
  },
};

const TRANSPARENT = 'rgba(0, 0, 0, 0)';

const RANGES: { key: string; days: number | null }[] = [
  { key: '1M', days: 30 },
  { key: '3M', days: 91 },
  { key: '6M', days: 182 },
  { key: '1Y', days: 365 },
  { key: 'All', days: null },
];

const DAY_MS = 86_400_000;

const PERIOD_LABELS: Record<'day' | 'week' | 'month', string> = {
  day: '日线',
  week: '周线',
  month: '月线',
};

type Candle = ChartCandlesPayload['candles'][number];

function movingAverage(closes: number[], n: number): (number | null)[] {
  const out: (number | null)[] = new Array<number | null>(closes.length).fill(
    null,
  );
  let sum = 0;
  for (let i = 0; i < closes.length; i++) {
    sum += closes[i];
    if (i >= n) sum -= closes[i - n];
    if (i >= n - 1) out[i] = sum / n;
  }
  return out;
}

function formatUtcDate(tsMs: number): string {
  const d = new Date(tsMs);
  const m = String(d.getUTCMonth() + 1).padStart(2, '0');
  const day = String(d.getUTCDate()).padStart(2, '0');
  return `${d.getUTCFullYear()}-${m}-${day}`;
}

function formatVolume(v: number): string {
  if (v >= 1e8) return (v / 1e8).toFixed(2) + '亿';
  if (v >= 1e4) return (v / 1e4).toFixed(0) + '万';
  return String(Math.round(v));
}

interface HoverInfo {
  tsMs: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume?: number;
  x: number;
}

export function ReportCandlesBlock({
  payload,
}: {
  payload: ChartCandlesPayload;
}) {
  const { resolved: theme } = useTheme();
  const palette = PALETTES[theme];
  const [rangeKey, setRangeKey] = useState('All');
  const [hover, setHover] = useState<HoverInfo | null>(null);
  const [failed, setFailed] = useState(false);
  const chartHostRef = useRef<HTMLDivElement | null>(null);

  // Sort ascending, then dedupe on the SECOND (lightweight-charts keys bars
  // by UTCTimestamp seconds and throws on duplicate/unordered times) —
  // same-second candles keep the last one.
  const sorted = useMemo(() => {
    const asc = payload.candles.slice().sort((a, b) => a[0] - b[0]);
    const out: Candle[] = [];
    for (const c of asc) {
      const sec = Math.floor(c[0] / 1000);
      if (
        out.length > 0 &&
        Math.floor(out[out.length - 1][0] / 1000) === sec
      ) {
        out[out.length - 1] = c;
      } else {
        out.push(c);
      }
    }
    return out;
  }, [payload.candles]);

  const overlays = payload.overlays ?? [];
  const showMa20 = overlays.includes('ma20');
  const showMa60 = overlays.includes('ma60');

  // MAs are computed over the FULL inline series, then sliced together with
  // the candles — so the MA value at the window's left edge stays correct
  // after a range switch.
  const { ma20, ma60 } = useMemo(() => {
    const closes = sorted.map((c) => c[4]);
    return {
      ma20: showMa20 ? movingAverage(closes, 20) : null,
      ma60: showMa60 ? movingAverage(closes, 60) : null,
    };
  }, [sorted, showMa20, showMa60]);

  const days = RANGES.find((r) => r.key === rangeKey)?.days ?? null;
  const startIndex = useMemo(() => {
    if (days == null || sorted.length === 0) return 0;
    const cutoff = sorted[sorted.length - 1][0] - days * DAY_MS;
    const idx = sorted.findIndex((c) => c[0] >= cutoff);
    // A window with fewer than 2 bars is not a chart — fall back to All.
    if (idx < 0 || sorted.length - idx < 2) return 0;
    return idx;
  }, [sorted, days]);

  const visible = useMemo(
    () => sorted.slice(startIndex),
    [sorted, startIndex],
  );
  const hasVolume = useMemo(
    () => visible.some((c: Candle) => typeof c[5] === 'number'),
    [visible],
  );

  useEffect(() => {
    const host = chartHostRef.current;
    if (!host || visible.length === 0) return;

    // Crash containment: a throwing chart build (bad data the schema could
    // not foresee, library edge case) degrades this block to a placeholder
    // instead of taking down the whole report page.
    let created: ReturnType<typeof createChart> | null = null;
    let crosshairHandler: ((param: MouseEventParams) => void) | null = null;
    try {
    const chart = createChart(host, {
      autoSize: true,
      height: 300,
      layout: {
        background: { type: ColorType.Solid, color: TRANSPARENT },
        textColor: palette.axisText,
        fontSize: 10,
        attributionLogo: false,
      },
      grid: {
        vertLines: { visible: false },
        horzLines: { color: palette.grid },
      },
      rightPriceScale: { borderVisible: false },
      timeScale: { borderVisible: false, timeVisible: false },
      handleScroll: false,
      handleScale: false,
    });
    created = chart;

    const candleSeries = chart.addSeries(CandlestickSeries, {
      // Hollow rising candle: transparent body, colored border + wick.
      upColor: TRANSPARENT,
      borderUpColor: palette.up,
      wickUpColor: palette.up,
      // Solid falling candle.
      downColor: palette.down,
      borderDownColor: palette.down,
      wickDownColor: palette.down,
      borderVisible: true,
    });
    candleSeries.setData(
      visible.map((c: Candle) => ({
        time: Math.floor(c[0] / 1000) as UTCTimestamp,
        open: c[1],
        high: c[2],
        low: c[3],
        close: c[4],
      })),
    );

    const maLine = (values: (number | null)[], color: string, dashed: boolean) => {
      const series = chart.addSeries(LineSeries, {
        color,
        lineWidth: 2,
        lineStyle: dashed ? LineStyle.Dashed : LineStyle.Solid,
        priceLineVisible: false,
        lastValueVisible: false,
        crosshairMarkerVisible: false,
      });
      series.setData(
        visible.flatMap((c: Candle, i: number) => {
          const v = values[startIndex + i];
          return v == null
            ? []
            : [{ time: Math.floor(c[0] / 1000) as UTCTimestamp, value: v }];
        }),
      );
    };
    if (ma20) maLine(ma20, palette.ma20, false);
    if (ma60) maLine(ma60, palette.ma60, true);

    if (hasVolume) {
      const volumeSeries = chart.addSeries(HistogramSeries, {
        priceScaleId: 'rb-volume',
        priceFormat: { type: 'volume' },
        priceLineVisible: false,
        lastValueVisible: false,
      });
      volumeSeries.priceScale().applyOptions({
        scaleMargins: { top: 0.82, bottom: 0 },
      });
      volumeSeries.setData(
        visible.flatMap((c: Candle) =>
          typeof c[5] === 'number'
            ? [
                {
                  time: Math.floor(c[0] / 1000) as UTCTimestamp,
                  value: c[5],
                  color:
                    (c[4] >= c[1] ? palette.up : palette.down) + '6b', // ~42% alpha
                },
              ]
            : [],
        ),
      );
    }

    const byTime = new Map<number, Candle>(
      visible.map((c: Candle) => [Math.floor(c[0] / 1000), c]),
    );
    const onCrosshair = (param: MouseEventParams) => {
      const t = typeof param.time === 'number' ? param.time : null;
      const candle = t != null ? byTime.get(t) : undefined;
      if (!candle || !param.point) {
        setHover(null);
        return;
      }
      setHover({
        tsMs: candle[0],
        open: candle[1],
        high: candle[2],
        low: candle[3],
        close: candle[4],
        volume: candle[5],
        x: param.point.x,
      });
    };
    chart.subscribeCrosshairMove(onCrosshair);
    crosshairHandler = onCrosshair;
    chart.timeScale().fitContent();
    } catch {
      try {
        created?.remove();
      } catch {
        // Already torn down.
      }
      setFailed(true);
      return;
    }

    return () => {
      if (created) {
        if (crosshairHandler) created.unsubscribeCrosshairMove(crosshairHandler);
        try {
          created.remove();
        } catch {
          // Already torn down.
        }
      }
      setHover(null);
    };
  }, [visible, startIndex, ma20, ma60, hasVolume, palette]);

  if (failed) {
    return (
      <div className="rb-unsupported" role="note">
        chart failed to render
      </div>
    );
  }

  const last = visible[visible.length - 1];
  const first = visible[0];
  const changePct =
    last && first && first[1] !== 0 ? (last[4] / first[1] - 1) * 100 : 0;
  const up = changePct >= 0;
  const periodLabel = PERIOD_LABELS[payload.period ?? 'day'];
  const hoverUp = hover != null && hover.close >= hover.open;

  return (
    <figure className="rb-fig" aria-label={`${payload.symbol} 蜡烛图`}>
      <div className="rb-fig-top">
        <span className="rb-fig-sym">{payload.symbol}</span>
        {last ? (
          <span className="rb-fig-last" data-testid="rb-fig-last">
            {last[4].toFixed(2)}
          </span>
        ) : null}
        <span
          className={'rb-fig-chg ' + (up ? 'rb-fig-chg--up' : 'rb-fig-chg--down')}
        >
          {(up ? '+' : '') + changePct.toFixed(2)}% · 区间
        </span>
        <span
          className="rb-ranges"
          role="group"
          aria-label="时间范围"
        >
          {RANGES.map((r) => (
            <button
              key={r.key}
              type="button"
              className="rb-range"
              aria-pressed={r.key === rangeKey}
              onClick={() => setRangeKey(r.key)}
            >
              {r.key}
            </button>
          ))}
        </span>
      </div>
      <div className="rb-chart">
        <div ref={chartHostRef} className="rb-chart-host" />
        {hover ? (
          <div
            className="rb-tip"
            style={{ left: Math.max(0, hover.x - 70) }}
            data-testid="rb-tip"
          >
            <div className="rb-tip-row">
              <span>{formatUtcDate(hover.tsMs)}</span>
            </div>
            <div className="rb-tip-row">
              <span>开</span>
              <span>{hover.open.toFixed(2)}</span>
            </div>
            <div className="rb-tip-row">
              <span>高</span>
              <span>{hover.high.toFixed(2)}</span>
            </div>
            <div className="rb-tip-row">
              <span>低</span>
              <span>{hover.low.toFixed(2)}</span>
            </div>
            <div className="rb-tip-row">
              <span>收</span>
              <span style={{ color: hoverUp ? 'var(--up)' : 'var(--down)' }}>
                {hover.close.toFixed(2)}
              </span>
            </div>
            {typeof hover.volume === 'number' ? (
              <div className="rb-tip-row">
                <span>量</span>
                <span>{formatVolume(hover.volume)}</span>
              </div>
            ) : null}
          </div>
        ) : null}
      </div>
      <figcaption className="rb-fig-cap">
        <span className="rb-legend" style={{ color: 'var(--up)' }}>
          <i className="rb-sw rb-sw--box" aria-hidden="true" />
          <span className="rb-legend-label">阳线</span>
        </span>
        <span className="rb-legend" style={{ color: 'var(--down)' }}>
          <i className="rb-sw rb-sw--box rb-sw--box-solid" aria-hidden="true" />
          <span className="rb-legend-label">阴线</span>
        </span>
        {showMa20 ? (
          <span className="rb-legend">
            <i
              className="rb-sw"
              style={{ background: palette.ma20 }}
              aria-hidden="true"
            />
            MA20
          </span>
        ) : null}
        {showMa60 ? (
          <span className="rb-legend">
            <i
              className="rb-sw"
              style={{ background: palette.ma60 }}
              aria-hidden="true"
            />
            MA60
          </span>
        ) : null}
        <span className="rb-fig-src">
          {[payload.caption, periodLabel].filter(Boolean).join(' · ')}
        </span>
      </figcaption>
    </figure>
  );
}
