import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ReportBlockView } from './index';
import type { ReportBlock } from '../../cards/builtins/wave-report';

// lightweight-charts draws on canvas — not available in jsdom. Mock the
// module surface (v5 API: `chart.addSeries(SeriesType, options)`) and record
// every series creation so tests can assert the exact config + data the
// renderer hands to the library.
const lw = vi.hoisted(() => {
  const state = {
    charts: 0,
    series: [] as {
      type: unknown;
      options: Record<string, unknown>;
      data: unknown[];
    }[],
    reset() {
      state.charts = 0;
      state.series = [];
    },
  };
  return state;
});

vi.mock('lightweight-charts', () => {
  const CandlestickSeries = { name: 'Candlestick' };
  const LineSeries = { name: 'Line' };
  const HistogramSeries = { name: 'Histogram' };
  return {
    CandlestickSeries,
    LineSeries,
    HistogramSeries,
    ColorType: { Solid: 'solid' },
    LineStyle: { Solid: 0, Dashed: 2 },
    createChart: () => {
      lw.charts += 1;
      return {
        addSeries: (type: unknown, options: Record<string, unknown>) => {
          const record = { type, options, data: [] as unknown[] };
          lw.series.push(record);
          return {
            setData: (data: unknown[]) => {
              record.data = data;
            },
            priceScale: () => ({ applyOptions: () => {} }),
          };
        },
        subscribeCrosshairMove: () => {},
        unsubscribeCrosshairMove: () => {},
        timeScale: () => ({ fitContent: () => {} }),
        remove: () => {},
      };
    },
  };
});

vi.mock('../../app/theme', () => ({
  useTheme: () => ({ mode: 'light', resolved: 'light', setMode: () => {} }),
}));

// The app block mounts a best-effort AppBridge; its handshake never
// completes in jsdom. Stub the module so tests stay deterministic.
vi.mock('@modelcontextprotocol/ext-apps/app-bridge', () => ({
  AppBridge: class {
    oncalltool: unknown;
    connect() {
      return new Promise(() => {});
    }
    close() {
      return Promise.resolve();
    }
    setHostContext() {}
  },
  PostMessageTransport: class {
    close() {
      return Promise.resolve();
    }
  },
}));

const DAY_MS = 86_400_000;
const T0 = Date.UTC(2026, 0, 5);

/** `n` daily candles ending at T0 + (n-1) days; close rises by 1 each day. */
function makeCandles(n: number, withVolume = false): number[][] {
  return Array.from({ length: n }, (_, i) => {
    const open = 100 + i;
    const close = open + 1;
    const base = [T0 + i * DAY_MS, open, close + 1, open - 1, close];
    return withVolume ? [...base, 1_000_000 + i] : base;
  });
}

function chartBlock(
  payload: Record<string, unknown>,
  id = 'b_chart',
): ReportBlock {
  return { id, kind: 'chart.candles', rev: 1, payload } as ReportBlock;
}

function candleSeriesRecords() {
  return lw.series.filter(
    (s) => (s.type as { name?: string }).name === 'Candlestick',
  );
}

beforeEach(() => {
  lw.reset();
});

describe('chart.candles block', () => {
  it('renders a hollow-up red/green candlestick series from inline data', async () => {
    render(
      <ReportBlockView
        block={chartBlock({
          symbol: '0700.HK',
          period: 'day',
          candles: makeCandles(30),
          caption: 'longbridge:kline',
        })}
      />,
    );

    expect(await screen.findByText('0700.HK')).toBeInTheDocument();
    expect(lw.charts).toBe(1);

    const [candles] = candleSeriesRecords();
    expect(candles).toBeDefined();
    // 红涨绿跌 + hollow rising body (transparent fill, colored border).
    expect(candles.options).toMatchObject({
      upColor: 'rgba(0, 0, 0, 0)',
      borderUpColor: '#b3271c',
      wickUpColor: '#b3271c',
      downColor: '#2ba471',
      borderDownColor: '#2ba471',
      borderVisible: true,
    });
    expect(candles.data).toHaveLength(30);
    expect(candles.data[0]).toEqual({
      time: T0 / 1000,
      open: 100,
      high: 102,
      low: 99,
      close: 101,
    });

    // Header: last close + positive range change, legend swatches.
    expect(screen.getByTestId('rb-fig-last')).toHaveTextContent('130.00');
    expect(screen.getByText('阳线')).toBeInTheDocument();
    expect(screen.getByText('阴线')).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'All' }),
    ).toHaveAttribute('aria-pressed', 'true');
  });

  it('filters candles client-side when a range is selected', async () => {
    render(
      <ReportBlockView
        block={chartBlock({ symbol: '0700.HK', candles: makeCandles(400) })}
      />,
    );
    await screen.findByText('0700.HK');
    expect(candleSeriesRecords().at(-1)?.data).toHaveLength(400);

    await userEvent.click(screen.getByRole('button', { name: '1M' }));
    const last = candleSeriesRecords().at(-1);
    // 30 days of daily candles → 31 bars inside the window.
    expect(last?.data).toHaveLength(31);
    expect(
      screen.getByRole('button', { name: '1M' }),
    ).toHaveAttribute('aria-pressed', 'true');
  });

  it('computes MA overlays client-side over the full series', async () => {
    render(
      <ReportBlockView
        block={chartBlock({
          symbol: '9988.HK',
          candles: makeCandles(25),
          overlays: ['ma20'],
        })}
      />,
    );
    await screen.findByText('9988.HK');

    const line = lw.series.find(
      (s) => (s.type as { name?: string }).name === 'Line',
    );
    expect(line).toBeDefined();
    expect(line?.options).toMatchObject({ color: '#3a6ecf' });
    // MA20 over 25 closes → defined from index 19 → 6 points.
    expect(line?.data).toHaveLength(6);
    const firstPoint = line?.data[0] as { value: number };
    // closes are 101..125; MA20 at index 19 = mean(101..120) = 110.5
    expect(firstPoint.value).toBeCloseTo(110.5);
    expect(screen.getByText('MA20')).toBeInTheDocument();
  });

  it('adds a volume histogram only when candles carry volume', async () => {
    render(
      <ReportBlockView
        block={chartBlock({ symbol: 'VOL', candles: makeCandles(10, true) })}
      />,
    );
    await screen.findByText('VOL');
    const histogram = lw.series.find(
      (s) => (s.type as { name?: string }).name === 'Histogram',
    );
    expect(histogram).toBeDefined();
    expect(histogram?.data).toHaveLength(10);
  });
});

describe('table block', () => {
  const tableBlockData: ReportBlock = {
    id: 'b_table',
    kind: 'table',
    rev: 1,
    payload: {
      caption: '可比公司 · 经调整口径',
      highlight: '腾讯控股',
      columns: [
        { key: 'name', label: '公司' },
        { key: 'pe', label: '动态 PE', align: 'right' },
        { key: 'chg', label: '近一年', align: 'right' },
      ],
      rows: [
        { name: '腾讯控股', pe: '18.4×', chg: '+21.4%' },
        { name: '阿里巴巴', pe: '13.1×', chg: null },
      ],
    },
  } as ReportBlock;

  it('renders columns with alignment, highlight row, and caption', () => {
    render(<ReportBlockView block={tableBlockData} />);

    const table = screen.getByRole('table', { name: /可比公司/ });
    const peHeader = within(table).getByRole('columnheader', {
      name: '动态 PE',
    });
    expect(peHeader).toHaveClass('rb-align-right');
    expect(
      within(table).getByRole('columnheader', { name: '公司' }),
    ).toHaveClass('rb-align-left');

    const highlighted = within(table)
      .getByRole('cell', { name: '腾讯控股' })
      .closest('tr');
    expect(highlighted).toHaveClass('rb-row-hi');
    const other = within(table)
      .getByRole('cell', { name: '阿里巴巴' })
      .closest('tr');
    expect(other).not.toHaveClass('rb-row-hi');
    // null cell renders empty, not "null".
    expect(within(table).queryByText('null')).not.toBeInTheDocument();
  });
});

describe('app block', () => {
  it('renders a sandboxed same-origin iframe with controlled height', () => {
    render(
      <ReportBlockView
        block={
          {
            id: 'b_app',
            kind: 'app',
            rev: 1,
            payload: { src: '/api/plugins/quotes/resources/orderbook', title: 'Orderbook', height: 240 },
          } as ReportBlock
        }
      />,
    );

    const iframe = screen.getByTitle('Orderbook');
    expect(iframe.tagName).toBe('IFRAME');
    expect(iframe).toHaveAttribute(
      'src',
      '/api/plugins/quotes/resources/orderbook',
    );
    expect(iframe).toHaveAttribute(
      'sandbox',
      'allow-scripts allow-same-origin',
    );
    expect(iframe).toHaveStyle({ height: '240px' });
    expect(screen.getByText('SANDBOXED')).toBeInTheDocument();
  });

  it('clamps height into the 120..2000 window and falls back to src as title', () => {
    render(
      <ReportBlockView
        block={
          {
            id: 'b_app2',
            kind: 'app',
            rev: 1,
            payload: { src: '/embed/x' },
          } as ReportBlock
        }
      />,
    );
    const iframe = screen.getByTitle('/embed/x');
    expect(iframe).toHaveStyle({ height: '360px' });
  });

  it('degrades a protocol-relative src to the unsupported placeholder', () => {
    render(
      <ReportBlockView
        block={
          {
            id: 'b_app3',
            kind: 'app',
            rev: 1,
            payload: { src: '//evil.example/x' },
          } as ReportBlock
        }
      />,
    );
    expect(screen.getByRole('note')).toHaveTextContent(
      'unsupported block kind app',
    );
  });
});

describe('degraded blocks', () => {
  it('renders a placeholder for an unknown kind without crashing', () => {
    render(
      <ReportBlockView
        block={{ id: 'b_x', kind: 'sparkline', rev: 1, payload: {} }}
      />,
    );
    expect(screen.getByRole('note')).toHaveTextContent(
      'unsupported block kind sparkline',
    );
  });

  it('renders a placeholder for a known kind with a malformed payload', () => {
    render(
      <ReportBlockView
        block={
          {
            id: 'b_bad',
            kind: 'chart.candles',
            rev: 1,
            // only one candle — schema requires >= 2
            payload: { symbol: 'X', candles: [[T0, 1, 2, 0.5, 1.5]] },
          } as ReportBlock
        }
      />,
    );
    expect(screen.getByRole('note')).toHaveTextContent(
      'unsupported block kind chart.candles',
    );
  });
});
