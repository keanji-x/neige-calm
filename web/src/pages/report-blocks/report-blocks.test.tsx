import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ReportBlockView } from './index';
import { ReportAppBlock } from './app';
import type { ReportBlock } from '../../cards/builtins/wave-report';

vi.mock('@tanstack/react-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tanstack/react-router')>();
  return {
    ...actual,
    Link: ({
      params,
      hash,
      children,
    }: {
      params: { waveId: string };
      hash?: string;
      children: React.ReactNode;
    }) => (
      <a href={`/wave/${params.waveId}${hash ? `#${hash}` : ''}`}>{children}</a>
    ),
  };
});

// lightweight-charts draws on canvas — not available in jsdom. Mock the
// module surface (v5 API: `chart.addSeries(SeriesType, options)`) and record
// every series creation so tests can assert the exact config + data the
// renderer hands to the library.
const lw = vi.hoisted(() => {
  const state = {
    charts: 0,
    throwOnCreate: false,
    series: [] as {
      type: unknown;
      options: Record<string, unknown>;
      data: unknown[];
    }[],
    reset() {
      state.charts = 0;
      state.throwOnCreate = false;
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
      if (lw.throwOnCreate) throw new Error('boom');
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

describe('prose block', () => {
  it('stamps deterministic ids onto H1 and H2 in document order', () => {
    render(
      <StrictMode>
        <ReportBlockView
          block={{
            id: 'b_multi',
            kind: 'prose',
            rev: 1,
            payload: { markdown: '# First\n\nBody\n\n## Second\n\n# Third' },
          }}
        />
      </StrictMode>,
    );

    expect(screen.getAllByRole('heading').map((heading) => heading.id)).toEqual([
      'b_multi-h1',
      'b_multi-h2',
      'b_multi-h3',
    ]);
  });
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
    // Default range is 1Y (#960 follow-up); with only 30 days of data the
    // window naturally covers the whole series.
    expect(
      screen.getByRole('button', { name: '1Y' }),
    ).toHaveAttribute('aria-pressed', 'true');
  });

  it('filters candles client-side when a range is selected', async () => {
    render(
      <ReportBlockView
        block={chartBlock({ symbol: '0700.HK', candles: makeCandles(400) })}
      />,
    );
    await screen.findByText('0700.HK');
    // Default 1Y window over 400 daily candles → 366 bars (365 days + the
    // cutoff bar itself).
    expect(candleSeriesRecords().at(-1)?.data).toHaveLength(366);

    await userEvent.click(screen.getByRole('button', { name: 'All' }));
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

  it('dedupes same-second candles keeping the last one', async () => {
    render(
      <ReportBlockView
        block={chartBlock({
          symbol: 'DUP',
          candles: [
            [T0, 100, 102, 99, 101],
            [T0 + 500, 100, 103, 99, 102], // same floor(ts/1000) second
            [T0 + DAY_MS, 102, 104, 101, 103],
          ],
        })}
      />,
    );
    await screen.findByText('DUP');

    const candles = candleSeriesRecords().at(-1);
    expect(candles?.data).toHaveLength(2);
    // The later same-second candle wins.
    expect(candles?.data[0]).toMatchObject({ close: 102, high: 103 });
  });

  it('falls back to All when a range window holds fewer than 2 bars', async () => {
    render(
      <ReportBlockView
        block={chartBlock({
          symbol: 'GAP',
          candles: [
            [T0, 100, 102, 99, 101],
            [T0 + 400 * DAY_MS, 101, 103, 100, 102],
          ],
        })}
      />,
    );
    await screen.findByText('GAP');

    await userEvent.click(screen.getByRole('button', { name: '1M' }));
    // Only the last candle sits inside 1M — the chart keeps all bars.
    expect(candleSeriesRecords().at(-1)?.data).toHaveLength(2);
  });

  it('degrades to a placeholder when the chart build throws', async () => {
    lw.throwOnCreate = true;
    render(
      <ReportBlockView
        block={chartBlock({ symbol: 'BOOM', candles: makeCandles(5) })}
      />,
    );
    expect(await screen.findByRole('note')).toHaveTextContent(
      'chart failed to render',
    );
  });

  it('retries the chart when a new payload arrives after a failure', async () => {
    lw.throwOnCreate = true;
    const { rerender } = render(
      <ReportBlockView
        block={chartBlock({ symbol: 'RETRY', candles: makeCandles(5) })}
      />,
    );
    expect(await screen.findByRole('note')).toHaveTextContent(
      'chart failed to render',
    );
    expect(lw.charts).toBe(0);

    lw.throwOnCreate = false;
    rerender(
      <ReportBlockView
        block={chartBlock({ symbol: 'RETRY', candles: makeCandles(6) })}
      />,
    );

    // New candle data clears the failure latch and rebuilds successfully.
    expect(await screen.findByTestId('rb-fig-last')).toHaveTextContent('106.00');
    expect(screen.queryByRole('note')).not.toBeInTheDocument();
    expect(lw.charts).toBe(1);
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

describe('prose report links', () => {
  const prose = (markdown: string): ReportBlock =>
    ({
      id: 'b_prose',
      kind: 'prose',
      rev: 1,
      payload: { markdown },
    }) as ReportBlock;

  it('renders a neige wave link as an in-app link with its block anchor', () => {
    render(
      <ReportBlockView
        block={prose('[Source](neige://wave/wave_2#b_cafe)')}
      />,
    );
    expect(screen.getByRole('link', { name: 'Source' })).toHaveAttribute(
      'href',
      '/wave/wave_2#b_cafe',
    );
  });

  it('degrades an invalid fragment to a whole-report link', () => {
    render(
      <ReportBlockView
        block={prose('[Source](neige://wave/wave_2#section)')}
      />,
    );
    expect(screen.getByRole('link', { name: 'Source' })).toHaveAttribute(
      'href',
      '/wave/wave_2',
    );
  });

  it('leaves a non-neige link unchanged', () => {
    render(
      <ReportBlockView block={prose('[Docs](https://example.com/guide)')} />,
    );
    expect(screen.getByRole('link', { name: 'Docs' })).toHaveAttribute(
      'href',
      'https://example.com/guide',
    );
  });

  it('still neutralises javascript links', () => {
    const { container } = render(
      <ReportBlockView block={prose('[Unsafe](javascript:alert(1))')} />,
    );
    const anchor = container.querySelector('a');
    expect(anchor).not.toBeNull();
    expect(anchor).toHaveAttribute('href', '');
  });

  it('neutralises invalid neige destinations', () => {
    const { container } = render(
      <ReportBlockView block={prose('[Unsafe](neige:javascript:alert(1))')} />,
    );
    const anchor = container.querySelector('a');
    expect(anchor).not.toBeNull();
    expect(anchor).toHaveAttribute('href', '');
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

  it('rejects a backslash src at the schema layer', () => {
    render(
      <ReportBlockView
        block={
          {
            id: 'b_app_bs',
            kind: 'app',
            rev: 1,
            payload: { src: '/\\evil.example/x' },
          } as ReportBlock
        }
      />,
    );
    expect(screen.getByRole('note')).toHaveTextContent(
      'unsupported block kind app',
    );
    expect(document.querySelector('iframe')).toBeNull();
  });

  it('asserts the resolved origin before mounting the iframe (defense in depth)', () => {
    // Bypass the zod layer on purpose — this exercises app.tsx's own check.
    render(
      <ReportAppBlock
        payload={{ src: '//evil.example/x' } as { src: string }}
      />,
    );
    expect(screen.getByRole('note')).toHaveTextContent(
      'unsupported block kind app',
    );
    expect(document.querySelector('iframe')).toBeNull();
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

  it('enforces the Rust-parity caps and strict shapes', () => {
    const cases: { name: string; block: ReportBlock }[] = [
      {
        name: 'chart: empty symbol',
        block: {
          id: 'c1',
          kind: 'chart.candles',
          rev: 1,
          payload: { symbol: '', candles: makeCandles(3) },
        } as ReportBlock,
      },
      {
        name: 'chart: over 5000 candles',
        block: {
          id: 'c2',
          kind: 'chart.candles',
          rev: 1,
          payload: { symbol: 'X', candles: makeCandles(5001) },
        } as ReportBlock,
      },
      {
        name: 'chart: unknown payload key (strict)',
        block: {
          id: 'c3',
          kind: 'chart.candles',
          rev: 1,
          payload: { symbol: 'X', candles: makeCandles(3), extra: 1 },
        } as ReportBlock,
      },
      {
        name: 'table: over 32 columns',
        block: {
          id: 't1',
          kind: 'table',
          rev: 1,
          payload: {
            columns: Array.from({ length: 33 }, (_, i) => ({
              key: `k${i}`,
              label: `L${i}`,
            })),
            rows: [],
          },
        } as ReportBlock,
      },
      {
        name: 'table: duplicate column keys',
        block: {
          id: 't2',
          kind: 'table',
          rev: 1,
          payload: {
            columns: [
              { key: 'k', label: 'A' },
              { key: 'k', label: 'B' },
            ],
            rows: [],
          },
        } as ReportBlock,
      },
      {
        name: 'table: row key outside declared columns',
        block: {
          id: 't3',
          kind: 'table',
          rev: 1,
          payload: {
            columns: [{ key: 'k', label: 'A' }],
            rows: [{ k: 'ok', stray: 'nope' }],
          },
        } as ReportBlock,
      },
      {
        name: 'table: over 500 rows',
        block: {
          id: 't4',
          kind: 'table',
          rev: 1,
          payload: {
            columns: [{ key: 'k', label: 'A' }],
            rows: Array.from({ length: 501 }, (_, i) => ({ k: i })),
          },
        } as ReportBlock,
      },
      {
        name: 'app: src over 2048 chars',
        block: {
          id: 'a1',
          kind: 'app',
          rev: 1,
          payload: { src: '/' + 'a'.repeat(2048) },
        } as ReportBlock,
      },
      {
        name: 'app: src with a C0 control character',
        block: {
          id: 'a2',
          kind: 'app',
          rev: 1,
          payload: { src: '/ok\u0007bell' },
        } as ReportBlock,
      },
      {
        name: 'app: src with a C1 control character',
        block: {
          id: 'a3',
          kind: 'app',
          rev: 1,
          payload: { src: '/ok\u0085next-line' },
        } as ReportBlock,
      },
      {
        name: 'table: string cell over 2048 chars',
        block: {
          id: 't5',
          kind: 'table',
          rev: 1,
          payload: {
            columns: [{ key: 'k', label: 'A' }],
            rows: [{ k: 'x'.repeat(2049) }],
          },
        } as ReportBlock,
      },
    ];

    for (const { name, block } of cases) {
      const { unmount } = render(<ReportBlockView block={block} />);
      expect(screen.getByRole('note'), name).toHaveTextContent(
        `unsupported block kind ${block.kind}`,
      );
      unmount();
    }
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
