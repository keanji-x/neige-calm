import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { WaveReportPage } from './WaveReportPage';
import {
  useOverlaysByKindQuery,
  useWaveBacklinksQuery,
  useWaveFileContent,
  useWaveFileList,
} from '../api/queries';
import { CalmApiError, type WaveFsContent, type WaveFsEntry } from '../api/calm';
import type { Wave, WaveCardSlot } from '../types';
import type { WaveReportCardData } from '../cards/builtins/wave-report';

vi.mock('../api/queries', () => ({
  useOverlaysByKindQuery: vi.fn(),
  useWaveBacklinksQuery: vi.fn(),
  useWaveFileList: vi.fn(),
  useWaveFileContent: vi.fn(),
}));

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
    useRouterState: <T,>({
      select,
    }: {
      select: (state: { location: { hash: string } }) => T;
    }) => select({ location: { hash: window.location.hash.slice(1) } }),
  };
});

// The spec-conversation panel's status dot reads `useCardOverlay`, which is
// React-Query-backed (REST-seeded card overlay snapshot) and would need a
// QueryClientProvider around every render below. The overlay value is
// irrelevant to this page's assertions (report body, files rail, event
// line), so stub the hook; its own behavior is covered in
// `src/cards/useCardOverlay.test.tsx`.
vi.mock('../cards/useCardOverlay', () => ({
  useCardOverlay: vi.fn(() => null),
}));

vi.mock('../app/theme', () => ({
  useTheme: () => ({
    mode: 'light',
    resolved: 'light',
    setMode: () => {},
  }),
}));

vi.mock('../cards/builtins/file-viewer-codemirror', () => ({
  CodePane: ({ text }: { text: string }) => (
    <pre data-testid="code-pane">{text}</pre>
  ),
}));

// Chart blocks lazy-load lightweight-charts, which needs a real canvas.
// Stub the v5 module surface; series assertions live in
// `report-blocks/report-blocks.test.tsx`.
vi.mock('lightweight-charts', () => ({
  CandlestickSeries: {},
  LineSeries: {},
  HistogramSeries: {},
  ColorType: { Solid: 'solid' },
  LineStyle: { Solid: 0, Dashed: 2 },
  createChart: () => ({
    addSeries: () => ({
      setData: () => {},
      priceScale: () => ({ applyOptions: () => {} }),
    }),
    subscribeCrosshairMove: () => {},
    unsubscribeCrosshairMove: () => {},
    timeScale: () => ({ fitContent: () => {} }),
    remove: () => {},
  }),
}));

const mockUseWaveFileList = vi.mocked(useWaveFileList);
const mockUseWaveFileContent = vi.mocked(useWaveFileContent);
const mockUseWaveBacklinksQuery = vi.mocked(useWaveBacklinksQuery);
const mockUseOverlaysByKindQuery = vi.mocked(useOverlaysByKindQuery);

const REPORT_RAIL_COLLAPSED_STORAGE_KEY = 'calm:report-rail:collapsed';
const REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY =
  'calm:report-rail:backlinks:collapsed';
const REPORT_FILES_COLLAPSED_STORAGE_KEY = 'calm:report-rail:files:collapsed';
const REPORT_CONVERSATION_COLLAPSED_STORAGE_KEY =
  'calm:report-conversation:collapsed';

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'wave_1',
    coveId: 'cove_1',
    title: 'Spec wave',
    lifecycle: 'draft',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    createdAt: 0,
    terminalAt: null,
    pinnedAt: null,
    cards: [],
    ...overrides,
  };
}

function reportSlot(
  body: string,
  overrides: Partial<WaveReportCardData> & { sort?: number } = {},
): WaveCardSlot {
  const { sort, ...cardOverrides } = overrides;
  const card: WaveReportCardData = {
    type: 'wave-report',
    id: 'report_1',
    summary: '',
    body,
  };
  return {
    kind: 'card',
    card: { ...card, ...cardOverrides },
    sort,
    deletable: false,
  };
}

function specSlot(id = 'card_spec_1'): WaveCardSlot {
  return {
    kind: 'card',
    card: {
      type: 'spec',
      id,
      goal: 'Answer follow-up questions',
    },
    sort: 0,
    deletable: false,
  };
}

function contentResult(
  value: Partial<ReturnType<typeof useWaveFileContent>> = {},
) {
  return {
    data: undefined,
    error: null,
    isLoading: false,
    ...value,
  } as unknown as ReturnType<typeof useWaveFileContent>;
}

function mockWaveFileContentForPath(
  path: string,
  value: Partial<ReturnType<typeof useWaveFileContent>>,
) {
  mockUseWaveFileContent.mockImplementation((_, requestedPath) => {
    if (requestedPath === path) {
      return contentResult(value);
    }
    return contentResult();
  });
}

function mockWaveFileContents(contents: Record<string, WaveFsContent>) {
  mockUseWaveFileContent.mockImplementation((_, requestedPath) => {
    const data = requestedPath ? contents[requestedPath] : undefined;
    return contentResult(data ? { data } : undefined);
  });
}

function mockWaveFileLists(lists: Record<string, WaveFsEntry[]>) {
  mockUseWaveFileList.mockImplementation((_, requestedPath = '') => {
    const path = requestedPath ?? '';
    return {
      data: lists[path],
      error: null,
      isLoading: false,
    } as unknown as ReturnType<typeof useWaveFileList>;
  });
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  delete (Element.prototype as Partial<Element>).scrollIntoView;
  window.localStorage.clear();
  window.history.replaceState(null, '', window.location.pathname);
});

describe('WaveReportPage', () => {
  beforeEach(() => {
    mockUseWaveBacklinksQuery.mockReturnValue({
      data: { backlinks: [], truncated: false, skipped_sources: 0 },
      error: null,
    } as unknown as ReturnType<typeof useWaveBacklinksQuery>);
    mockUseOverlaysByKindQuery.mockReturnValue({
      data: [],
    } as unknown as ReturnType<typeof useOverlaysByKindQuery>);
    const files: WaveFsEntry[] = [
      { name: 'report.md', kind: 'file' },
      { name: 'wave.json', kind: 'file' },
    ];
    mockUseWaveFileList.mockReturnValue({
      data: files,
      error: null,
      isLoading: false,
    } as unknown as ReturnType<typeof useWaveFileList>);
    mockUseWaveFileContent.mockImplementation((_, requestedPath) => {
      if (requestedPath === 'report.md') {
        return contentResult({
          error: new CalmApiError(404, 'not_found', 'File not found'),
        });
      }
      return contentResult();
    });
  });

  it('renders the empty state when there is no report card and report.md is missing', () => {
    render(<WaveReportPage wave={makeWave()} cards={[]} />);

    expect(
      screen.getByText(
        'Report not ready. The spec agent has not produced a report yet.',
      ),
    ).toBeInTheDocument();
  });

  it('skips the report.md fetch when there is no report card', () => {
    mockUseWaveFileContent.mockClear();

    render(<WaveReportPage wave={makeWave()} cards={[]} />);

    const reportMdCall = mockUseWaveFileContent.mock.calls.find(
      (args) => args[1] === 'report.md',
    );
    expect(reportMdCall).toBeUndefined();
  });

  it('renders a non-report file even when the wave has no report card', async () => {
    mockUseWaveFileContent.mockImplementation((_, requestedPath) => {
      if (requestedPath === 'wave.json') {
        return contentResult({
          data: { content_type: 'text/plain', content: 'plain text' },
        });
      }
      return contentResult();
    });

    render(<WaveReportPage wave={makeWave()} cards={[]} />);

    fireEvent.click(screen.getByRole('treeitem', { name: /wave\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      'plain text',
    );
  });

  it('renders the report in its own focusable scroll root', () => {
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('The **answer** is ready.')]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Spec wave' }),
    ).toBeInTheDocument();
    expect(screen.getByText('answer').tagName).toBe('STRONG');
    const scrollRoot = screen.getByLabelText('Report document');
    expect(scrollRoot).toHaveClass('report-document-scroll');
    expect(scrollRoot).toHaveAttribute('tabindex', '0');
    expect(scrollRoot.parentElement).toHaveClass('report-center');
    expect(container.querySelector('.report-doc')?.parentElement).toBe(
      scrollRoot,
    );
  });

  it('renders derived prose blocks when present', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('stale body', {
            blocks: [
              {
                id: 'b_0001',
                kind: 'prose',
                rev: 1,
                payload: { markdown: '# Block title\n\nBlock **body**' },
              },
            ],
          }),
        ]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Block title' }),
    ).toBeInTheDocument();
    expect(screen.getByText('body').tagName).toBe('STRONG');
    expect(screen.queryByText('stale body')).not.toBeInTheDocument();
  });

  it('numbers a three-prose-block report as 01/02/03 in one body scope', () => {
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      value: () => {},
    });
    const scrollIntoView = vi.spyOn(Element.prototype, 'scrollIntoView');
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('stale body', {
            blocks: [
              {
                id: 'b_1',
                kind: 'prose',
                rev: 1,
                payload: { markdown: '## First' },
              },
              {
                id: 'b_2',
                kind: 'prose',
                rev: 1,
                payload: { markdown: '## Second' },
              },
              {
                id: 'b_3',
                kind: 'prose',
                rev: 1,
                payload: { markdown: '## Third' },
              },
            ],
          }),
        ]}
      />,
    );

    const outline = screen.getByRole('region', { name: 'Outline' });
    expect(within(outline).getByRole('link', { name: '01 First' }))
      .toBeInTheDocument();
    expect(within(outline).getByRole('link', { name: '02 Second' }))
      .toBeInTheDocument();
    expect(within(outline).getByRole('link', { name: '03 Third' }))
      .toBeInTheDocument();
    const reportBody = container.querySelector('.report-body');
    expect(reportBody?.querySelectorAll(':scope > .report-prose')).toHaveLength(3);

    const first = within(outline).getByRole('link', { name: '01 First' });
    fireEvent.click(first);
    fireEvent.click(first);
    expect(scrollIntoView).toHaveBeenCalledTimes(2);
  });

  it('scrolls a multi-H2 outline link to its matching heading', async () => {
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      value: () => {},
    });
    const scrollIntoView = vi.spyOn(Element.prototype, 'scrollIntoView');
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('stale body', {
            blocks: [{
              id: 'b_multi',
              kind: 'prose',
              rev: 1,
              payload: { markdown: '## First\n\nFirst body\n\n## Second' },
            }],
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByRole('link', { name: '02 Second' }));

    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(scrollIntoView.mock.contexts[0]).toBe(
      document.getElementById('b_multi-h2'),
    );
    await waitFor(() => {
      expect(document.getElementById('b_multi'))
        .toHaveClass('report-block--highlight');
    });
  });

  it('shows a leading chart as an unnumbered top-level outline entry', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('stale body', {
            blocks: [
              {
                id: 'b_chart',
                kind: 'chart.candles',
                rev: 1,
                payload: { symbol: '0700.HK', candles: [] },
              },
              {
                id: 'b_section',
                kind: 'prose',
                rev: 1,
                payload: { markdown: '## Market' },
              },
            ],
          }),
        ]}
      />,
    );

    const outline = screen.getByRole('region', { name: 'Outline' });
    expect(within(outline).getByRole('link', { name: '0700.HK' }))
      .toHaveAttribute('href', '#b_chart');
    expect(within(outline).getByRole('link', { name: '01 Market' }))
      .toHaveAttribute('href', '#b_section-h1');
  });

  it('explains the body-only outline downgrade without inert links', () => {
    render(
      <WaveReportPage wave={makeWave()} cards={[reportSlot('## Flat heading')]} />,
    );

    const outline = screen.getByRole('region', { name: 'Outline' });
    expect(
      within(outline).getByText(
        'Outline navigation requires structured report blocks.',
      ),
    ).toBeInTheDocument();
    expect(within(outline).queryByRole('link')).toBeNull();
  });

  it('renders mixed blocks (prose + chart + table + prose) in order', async () => {
    const day = 86_400_000;
    const t0 = Date.UTC(2026, 0, 5);
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('stale body', {
            blocks: [
              {
                id: 'b_1',
                kind: 'prose',
                rev: 1,
                payload: { markdown: 'Opening paragraph' },
              },
              {
                id: 'b_2',
                kind: 'chart.candles',
                rev: 1,
                payload: {
                  symbol: '0700.HK',
                  candles: [
                    [t0, 100, 102, 99, 101],
                    [t0 + day, 101, 103, 100, 102],
                  ],
                },
              },
              {
                id: 'b_3',
                kind: 'table',
                rev: 1,
                payload: {
                  columns: [{ key: 'k', label: 'Key' }],
                  rows: [{ k: 'v1' }],
                },
              },
              {
                id: 'b_4',
                kind: 'prose',
                rev: 1,
                payload: { markdown: 'Closing paragraph' },
              },
            ],
          }),
        ]}
      />,
    );

    // Chart mounts through React.lazy — wait for its figure to appear.
    const reportBody = container.querySelector('.report-body');
    expect(reportBody).not.toBeNull();
    expect(await within(reportBody as HTMLElement).findByText('0700.HK'))
      .toBeInTheDocument();
    expect(screen.getByText('Opening paragraph')).toBeInTheDocument();
    expect(screen.getByText('Closing paragraph')).toBeInTheDocument();
    expect(screen.getByRole('table')).toBeInTheDocument();

    const article = container.querySelector('.report-doc');
    const blockEls = Array.from(
      article?.querySelectorAll('.report-block') ?? [],
    );
    expect(blockEls).toHaveLength(4);
    expect(blockEls[0]).toHaveTextContent('Opening paragraph');
    expect(blockEls[1]).toHaveClass('report-block--breakout');
    expect(blockEls[1]).toHaveTextContent('0700.HK');
    expect(blockEls[2]).toHaveClass('report-block--breakout');
    expect(blockEls[2]).toHaveTextContent('v1');
    expect(blockEls[3]).toHaveTextContent('Closing paragraph');
  });

  it('degrades an unknown block kind to a placeholder without dropping siblings', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('stale body', {
            blocks: [
              {
                id: 'b_1',
                kind: 'prose',
                rev: 1,
                payload: { markdown: 'Still here' },
              },
              { id: 'b_2', kind: 'holo.gram', rev: 1, payload: {} },
            ],
          }),
        ]}
      />,
    );

    expect(screen.getByText('Still here')).toBeInTheDocument();
    expect(screen.getByRole('note')).toHaveTextContent(
      'unsupported block kind holo.gram',
    );
  });

  it('shows an explicit unsupported-version state', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('', { unsupportedVersion: 2 })]}
      />,
    );

    expect(screen.getByRole('alert')).toHaveTextContent('版本不支持，请刷新');
    const outline = screen.getByRole('region', { name: 'Outline' });
    expect(
      within(outline).getByText('Outline unavailable for this report version.'),
    ).toBeInTheDocument();
    expect(within(outline).queryByRole('link')).toBeNull();
  });

  it('shows the duplicate banner and renders the lowest-sort report', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('Later body', { id: 'report_2', sort: 5 }),
          reportSlot('Earliest body', { id: 'report_1', sort: 1 }),
        ]}
      />,
    );

    expect(screen.getByRole('status')).toHaveTextContent(
      'Multiple report cards found. Showing the earliest.',
    );
    expect(screen.getByText('Earliest body')).toBeInTheDocument();
    expect(screen.queryByText('Later body')).not.toBeInTheDocument();
  });

  it('renders GFM tables and strikethrough', () => {
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('| Key | Value |\n| --- | --- |\n| A | B |\n\n~~stale~~'),
        ]}
      />,
    );

    const table = screen.getByRole('table');
    expect(within(table).getByRole('columnheader', { name: 'Key' })).toBeTruthy();
    expect(within(table).getByRole('cell', { name: 'B' })).toBeTruthy();
    expect(container.querySelector('del')).toHaveTextContent('stale');
  });

  it('shows a relative updatedAt byline when present', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('Fresh body', {
            updatedAt: new Date('2026-06-10T10:00:00Z').getTime(),
          }),
        ]}
      />,
    );

    expect(screen.getByLabelText('Report metadata')).toHaveTextContent(
      'Updated 2h ago',
    );
  });

  it('renders a real Files tree instead of the PR-B placeholder', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    expect(
      screen.getByRole('tree', { name: 'Wave files' }),
    ).toBeInTheDocument();
    expect(screen.getByRole('treeitem', { name: /report\.md/ })).toBeTruthy();
    expect(
      screen.queryByText('Wave files appear here. (Wired in PR-B.)'),
    ).not.toBeInTheDocument();
  });

  it('toggles dotfile rows in the Files rail without hiding siblings', async () => {
    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_hidden/', kind: 'dir' }],
      'cards/card_hidden': [
        { name: '.meta.json', kind: 'file' },
        { name: '.payload.json', kind: 'file' },
        { name: 'events.json', kind: 'file' },
      ],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: '[{"id":"card_hidden","kind":"codex"}]',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /codex card_hid/ }),
    );

    expect(screen.queryByRole('treeitem', { name: /\.meta\.json/ })).toBeNull();
    expect(
      screen.queryByRole('treeitem', { name: /\.payload\.json/ }),
    ).toBeNull();
    expect(screen.getByRole('treeitem', { name: /events\.json/ })).toBeTruthy();

    const showAll = screen.getByRole('button', { name: 'Show all' });
    expect(showAll).toHaveAttribute('aria-pressed', 'false');

    fireEvent.click(showAll);

    expect(showAll).toHaveAttribute('aria-pressed', 'true');
    expect(await screen.findByRole('treeitem', { name: /\.meta\.json/ }))
      .toBeTruthy();
    expect(screen.getByRole('treeitem', { name: /\.payload\.json/ }))
      .toBeTruthy();
    expect(screen.getByRole('treeitem', { name: /events\.json/ })).toBeTruthy();

    fireEvent.click(showAll);

    expect(showAll).toHaveAttribute('aria-pressed', 'false');
    expect(screen.queryByRole('treeitem', { name: /\.meta\.json/ })).toBeNull();
    expect(
      screen.queryByRole('treeitem', { name: /\.payload\.json/ }),
    ).toBeNull();
    expect(screen.getByRole('treeitem', { name: /events\.json/ })).toBeTruthy();
  });

  it('collapses and expands the Files rail from the rail toggle', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const rail = screen.getByLabelText('Report context');
    const collapseToggle = screen.getByRole('button', {
      name: 'Collapse report rail',
    });

    expect(collapseToggle).toHaveAttribute('aria-expanded', 'true');
    expect(rail).not.toHaveClass('report-rail--collapsed');
    expect(screen.getByRole('tree', { name: 'Wave files' })).toBeInTheDocument();

    fireEvent.click(collapseToggle);

    const expandToggle = screen.getByRole('button', {
      name: 'Expand report rail',
    });
    expect(expandToggle).toHaveAttribute('aria-expanded', 'false');
    expect(rail).toHaveClass('report-rail--collapsed');
    expect(screen.queryByRole('tree', { name: 'Wave files' })).toBeNull();
    expect(window.localStorage.getItem(REPORT_RAIL_COLLAPSED_STORAGE_KEY))
      .toBe('true');

    fireEvent.click(expandToggle);

    expect(screen.getByRole('button', { name: 'Collapse report rail' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(rail).not.toHaveClass('report-rail--collapsed');
    expect(screen.getByRole('tree', { name: 'Wave files' })).toBeInTheDocument();
    expect(window.localStorage.getItem(REPORT_RAIL_COLLAPSED_STORAGE_KEY))
      .toBe('false');
  });

  it('switches between the rail-top toggle and edge opener', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const toggle = screen.getByRole('button', {
      name: 'Collapse report rail',
    });
    toggle.focus();
    expect(toggle).toHaveFocus();

    fireEvent.click(toggle);

    const expandToggle = screen.getByRole('button', {
      name: 'Expand report rail',
    });
    expect(expandToggle).not.toBe(toggle);

    fireEvent.click(expandToggle);

    const collapseToggle = screen.getByRole('button', {
      name: 'Collapse report rail',
    });
    expect(collapseToggle).not.toBe(expandToggle);
  });

  it('persists the collapsed Files rail across remounts', () => {
    const props = {
      wave: makeWave(),
      cards: [reportSlot('Files rail body')],
    };
    const { unmount } = render(<WaveReportPage {...props} />);

    fireEvent.click(screen.getByRole('button', { name: 'Collapse report rail' }));
    unmount();
    render(<WaveReportPage {...props} />);

    expect(screen.getByRole('button', { name: 'Expand report rail' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByLabelText('Report context'))
      .toHaveClass('report-rail--collapsed');
    expect(screen.queryByRole('tree', { name: 'Wave files' })).toBeNull();
  });

  it('persists each collapsible rail section independently', () => {
    const props = {
      wave: makeWave(),
      cards: [reportSlot('Rail body')],
    };
    const { unmount } = render(<WaveReportPage {...props} />);

    fireEvent.click(screen.getByRole('button', { name: 'Collapse Backlinks' }));
    fireEvent.click(screen.getByRole('button', { name: 'Collapse Files' }));

    expect(
      window.localStorage.getItem(REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY),
    ).toBe('true');
    expect(
      window.localStorage.getItem(REPORT_FILES_COLLAPSED_STORAGE_KEY),
    ).toBe('true');

    unmount();
    render(<WaveReportPage {...props} />);
    expect(screen.getByRole('button', { name: 'Expand Backlinks' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByRole('button', { name: 'Expand Files' }))
      .toHaveAttribute('aria-expanded', 'false');
  });

  it('defaults the report rail to collapsed at the narrow-screen breakpoint', () => {
    vi.stubGlobal(
      'matchMedia',
      vi.fn(() => ({ matches: true })) as unknown as typeof window.matchMedia,
    );

    render(
      <WaveReportPage wave={makeWave()} cards={[reportSlot('Narrow body')]} />,
    );

    expect(screen.getByLabelText('Report context'))
      .toHaveClass('report-rail--collapsed');
    expect(screen.getByRole('button', { name: 'Expand report rail' }))
      .toHaveAttribute('aria-expanded', 'false');
  });

  it('renders the three rail sections in order with Event line removed', () => {
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Rail body')]}
      />,
    );

    const rail = screen.getByLabelText('Report context');
    expect(
      Array.from(rail.querySelectorAll(':scope > section')).map((section) =>
        section.getAttribute('aria-label'),
      ),
    ).toEqual(['Outline', 'Backlinks', 'Files']);
    expect(screen.queryByRole('region', { name: 'Event line' })).toBeNull();
    const page = container.querySelector('.report-page');
    expect(page?.firstElementChild).toBe(rail);
    expect(page?.children[1]).toHaveClass('report-rail-open');
    expect(page?.children[2]).toHaveClass('report-center');
    expect(page?.lastElementChild).toHaveClass('report-conversation-drawer');
  });

  it('defaults the main column to report.md content', () => {
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    expect(
      screen.getByRole('treeitem', { name: /report\.md/ }),
    ).toHaveAttribute('aria-selected', 'true');
    expect(
      screen.getByRole('heading', { level: 1, name: 'Hi' }),
    ).toBeInTheDocument();
    expect(mockUseWaveFileContent).toHaveBeenCalledWith('wave_1', 'report.md', {
      enabled: true,
    });
  });

  it('switches the main column to another selected file', async () => {
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'wave.json': {
        content_type: 'application/json',
        content: '{"ok":true}',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Hi' }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole('treeitem', { name: /wave\.json/ }));

    expect(
      screen.queryByRole('heading', { level: 1, name: 'Hi' }),
    ).not.toBeInTheDocument();
    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"ok":true}',
    );
  });

  it('switches back to report.md from the Files tree', async () => {
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'wave.json': {
        content_type: 'application/json',
        content: '{"ok":true}',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /wave\.json/ }));
    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"ok":true}',
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));

    expect(
      await screen.findByRole('heading', { level: 1, name: 'Hi' }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the cards/index.json wave fs viewer', async () => {
    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'index.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: JSON.stringify([
          {
            id: 'card_codex_1',
            kind: 'codex',
            role: 'worker',
            sort: 10,
            deletable: true,
            created_at: 100,
            updated_at: 200,
          },
          {
            id: 'card_report_1',
            kind: 'wave-report',
            role: 'reportcard',
            sort: 20,
            deletable: false,
            created_at: 300,
            updated_at: 400,
          },
        ]),
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(
      await screen.findByRole('heading', { name: 'Cards in this wave (2)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('codex')).toHaveClass(
      'wave-fs-viewer-card-title',
    );
    expect(screen.getByText('worker')).toHaveClass('wave-fs-viewer-chip');
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the wave.json wave fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'wave.json': {
        content_type: 'application/json',
        content: JSON.stringify({
          title: 'Wave fs registry',
          id: 'wave_json_1',
          cove_id: 'cove_json_1',
          lifecycle: 'working',
          cwd: '/repo/neige-calm',
          workflow_id: null,
          workflow_input: null,
          sort: 3,
          archived_at: null,
          pinned_at: new Date('2026-06-10T11:55:00Z').getTime(),
          terminal_at: null,
          created_at: 0,
          updated_at: 0,
        }),
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /wave\.json/ }));

    expect(
      await screen.findByRole('heading', { name: 'Wave fs registry' }),
    ).toHaveClass('wave-fs-viewer-primary');
    expect(screen.getByText('wave_json_1')).toHaveClass('wave-fs-viewer-mono');
    expect(screen.getByText('Pinned 5m ago')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the cards/<id>/.meta.json wave fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_meta/', kind: 'dir' }],
      'cards/card_meta': [{ name: '.meta.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: '[{"id":"card_meta","kind":"codex"}]',
      },
      'cards/card_meta/.meta.json': {
        content_type: 'application/json',
        content: JSON.stringify({
          id: 'card_meta',
          kind: 'codex',
          role: 'spec',
          sort: 5,
          deletable: false,
          created_at: new Date('2026-06-10T10:00:00Z').getTime(),
          updated_at: new Date('2026-06-10T11:55:00Z').getTime(),
        }),
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Show all' }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /codex card_met/ }),
    );
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /\.meta\.json/ }),
    );

    expect(await screen.findByRole('heading', { name: 'codex' })).toHaveClass(
      'wave-fs-viewer-primary',
    );
    expect(screen.getByText('spec')).toHaveClass('wave-fs-viewer-chip');
    expect(screen.getByText('deletable: no')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the cards/<id>/events.json wave fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_events/', kind: 'dir' }],
      'cards/card_events': [{ name: 'events.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: '[{"id":"card_events","kind":"codex"}]',
      },
      'cards/card_events/events.json': {
        content_type: 'application/json',
        content: JSON.stringify([
          {
            created_at: new Date('2026-06-10T11:40:00Z').getTime(),
            event_id: 2,
            kind: 'claude.hook',
            hook_kind: 'PostToolUse',
            payload: { tool: 'Read', ok: true },
          },
          {
            created_at: new Date('2026-06-10T11:00:00Z').getTime(),
            event_id: 1,
            kind: 'codex.hook',
            hook_kind: 'PreToolUse',
            payload: { tool: 'Bash' },
          },
        ]),
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /codex card_eve/ }),
    );
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /events\.json/ }),
    );

    expect(
      await screen.findByRole('heading', { name: 'Hook events (2)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('PreToolUse')).toHaveClass(
      'wave-fs-viewer-primary',
    );
    expect(screen.getByText('codex.hook')).toHaveAttribute(
      'data-tone',
      'accent',
    );
    expect(screen.getByText('Created 1h ago')).toBeInTheDocument();
    expect(screen.getAllByText('Payload')[0].closest('details'))
      .not.toHaveAttribute('open');
    expect(screen.getByText(/"tool": "Bash"/)).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the cards/<id>/runtime.json wave fs viewer', async () => {
    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_runtime/', kind: 'dir' }],
      'cards/card_runtime': [{ name: 'runtime.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: '[{"id":"card_runtime","kind":"claude"}]',
      },
      'cards/card_runtime/runtime.json': {
        content_type: 'application/json',
        content: JSON.stringify({
          runtime_id: 'runtime_page_1',
          kind: 'claude',
          status: 'turn_pending',
          provider: 'claude',
          terminal_id: 'terminal_page_1',
          thread_id: 'thread_page_1',
          session_id: 'session_page_1',
          source: 'wave-dispatcher',
          thread_status: 'queued',
        }),
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /claude card_run/ }),
    );
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /runtime\.json/ }),
    );

    expect(await screen.findByRole('heading', { name: 'claude' })).toHaveClass(
      'wave-fs-viewer-primary',
    );
    expect(screen.getByText('runtime_page_1')).toHaveClass(
      'wave-fs-viewer-mono',
    );
    expect(screen.getByText('turn_pending')).toHaveAttribute(
      'data-tone',
      'warning',
    );
    expect(screen.getByText('claude', { selector: '.wave-fs-viewer-chip' }))
      .toBeInTheDocument();
    expect(screen.getByText('terminal_page_1')).toHaveClass(
      'wave-fs-viewer-mono',
    );
    expect(screen.getByText('thread_page_1')).toHaveClass(
      'wave-fs-viewer-mono',
    );
    expect(screen.getByText('session_page_1')).toHaveClass(
      'wave-fs-viewer-mono',
    );
    expect(screen.getByText('wave-dispatcher')).toHaveClass(
      'wave-fs-viewer-mono',
    );
    expect(screen.getByText('queued')).toHaveClass('wave-fs-viewer-mono');
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the runs/index.json wave fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockWaveFileLists({
      '': [
        { name: 'report.md', kind: 'file' },
        { name: 'runs/', kind: 'dir', size: 1 },
      ],
      runs: [{ name: 'index.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'runs/index.json': {
        content_type: 'application/json',
        content: JSON.stringify([
          {
            idempotency_key: 'run_codex_1',
            status: 'completed',
            kind: 'codex',
            verdict: {
              status: 'accepted',
              at: new Date('2026-06-10T11:00:00Z').getTime(),
            },
            requested_at: new Date('2026-06-10T10:00:00Z').getTime(),
            finished_at: new Date('2026-06-10T11:00:00Z').getTime(),
            worker_card_id: 'card_worker_1',
          },
        ]),
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /runs\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(
      await screen.findByRole('heading', { name: 'Runs in this wave (1)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('run_codex_1')).toHaveClass(
      'wave-fs-viewer-mono',
    );
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the runs/<key>.json wave fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    const rawRunDetail = JSON.stringify({
      idempotency_key: 'run_terminal_1',
      status: 'failed',
      kind: 'terminal',
      verdict: {
        status: 'rejected',
        reason: 'Worker returned non-zero exit status',
        at: new Date('2026-06-10T11:00:00Z').getTime(),
      },
      requested_at: new Date('2026-06-10T10:00:00Z').getTime(),
      finished_at: new Date('2026-06-10T11:00:00Z').getTime(),
      worker_card_id: 'card_worker_2',
      events: {
        requested: {
          created_at: new Date('2026-06-10T10:00:00Z').getTime(),
          event_id: 1,
          kind: 'worker.requested',
          payload: { idempotency_key: 'run_terminal_1' },
        },
        completed: null,
        failed: null,
        verdict: null,
      },
      worker_card_payload: { idempotency_key: 'run_terminal_1' },
    });
    mockWaveFileLists({
      '': [
        { name: 'report.md', kind: 'file' },
        { name: 'runs/', kind: 'dir', size: 1 },
      ],
      runs: [{ name: 'run_terminal_1.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'runs/run_terminal_1.json': {
        content_type: 'application/json',
        content: rawRunDetail,
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /runs\// }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /run_terminal_1\.json/ }),
    );

    expect(
      await screen.findByRole('heading', { name: 'terminal' }),
    ).toHaveClass('wave-fs-viewer-primary');
    expect(screen.getByText('run_terminal_1')).toHaveClass(
      'wave-fs-viewer-mono',
    );
    expect(
      screen.getByText('Worker returned non-zero exit status'),
    ).toHaveClass('wave-fs-viewer-verdict-reason');
    const summary = screen.getByText('Full payload (events, worker card)');
    const details = summary.closest('details');
    expect(details).not.toHaveAttribute('open');
    expect(details?.querySelector('code')).toHaveTextContent(rawRunDetail);
    expect(
      screen.queryByText('events / payload: see raw JSON'),
    ).not.toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('falls back to raw JSON when a cards/<id>/.payload.json path has no registered viewer', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const rawPayload = JSON.stringify({
      opaque: true,
      body: { message: 'viewer intentionally unregistered' },
    });

    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_payload/', kind: 'dir' }],
      'cards/card_payload': [{ name: '.payload.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: '[{"id":"card_payload","kind":"codex"}]',
      },
      'cards/card_payload/.payload.json': {
        content_type: 'application/json',
        content: rawPayload,
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    const showAll = screen.getByRole('button', { name: 'Show all' });
    fireEvent.click(showAll);
    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /codex card_pay/ }),
    );
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /\.payload\.json/ }),
    );

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      rawPayload,
    );
    fireEvent.click(showAll);
    expect(
      screen.queryByRole('treeitem', { name: /\.payload\.json/ }),
    ).toBeNull();
    expect(screen.getByTestId('code-pane')).toHaveTextContent(rawPayload);
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('falls back to raw JSON when runs/index.json is malformed', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    mockWaveFileLists({
      '': [
        { name: 'report.md', kind: 'file' },
        { name: 'runs/', kind: 'dir', size: 1 },
      ],
      runs: [{ name: 'index.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'runs/index.json': {
        content_type: 'application/json',
        content: '{"not":"an array"}',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /runs\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"not":"an array"}',
    );
    expect(
      screen.queryByRole('heading', { name: /Runs in this wave/ }),
    ).not.toBeInTheDocument();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('falls back to raw JSON when cards/index.json is malformed', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'index.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: '{"not":"an array"}',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"not":"an array"}',
    );
    expect(
      screen.queryByRole('heading', { name: /Cards in this wave/ }),
    ).not.toBeInTheDocument();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('falls back to raw JSON when cards/index.json is invalid JSON', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'index.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: 'not valid json {{',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      'not valid json {{',
    );
    expect(
      screen.queryByRole('heading', { name: /Cards in this wave/ }),
    ).not.toBeInTheDocument();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('falls back to raw JSON when cards/<id>/runtime.json is malformed', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [
        { name: 'index.json', kind: 'file' },
        { name: 'card_x/', kind: 'dir' },
      ],
      'cards/card_x': [{ name: 'runtime.json', kind: 'file' }],
    });
    mockWaveFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'cards/index.json': {
        content_type: 'application/json',
        content: '[{"id":"card_x","kind":"codex"}]',
      },
      'cards/card_x/runtime.json': {
        content_type: 'application/json',
        content: '{"status":"running"}',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /codex card_x/ }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /runtime\.json/ }),
    );

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"status":"running"}',
    );
    expect(
      screen.queryByText('No runtime attached.'),
    ).not.toBeInTheDocument();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('resets the selected file to report.md when the wave id changes', async () => {
    mockUseWaveFileContent.mockImplementation((waveId, requestedPath) => {
      if (requestedPath === 'report.md') {
        return contentResult({
          data: {
            content_type: 'text/markdown',
            content: waveId === 'wave_B' ? '# New report' : '# Old report',
          },
        });
      }
      if (requestedPath === 'wave.json') {
        return contentResult({
          data: {
            content_type: 'application/json',
            content: '{"ok":true}',
          },
        });
      }
      return contentResult();
    });

    const { rerender } = render(
      <WaveReportPage
        wave={makeWave({ id: 'wave_A' })}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /wave\.json/ }));
    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"ok":true}',
    );

    rerender(
      <WaveReportPage
        wave={makeWave({ id: 'wave_B' })}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    await waitFor(() => {
      expect(
        screen.getByRole('treeitem', { name: /report\.md/ }),
      ).toHaveAttribute('aria-selected', 'true');
      expect(
        screen.getByRole('treeitem', { name: /wave\.json/ }),
      ).toHaveAttribute('aria-selected', 'false');
      expect(
        screen.getByRole('heading', { level: 1, name: 'New report' }),
      ).toBeInTheDocument();
    });
  });

  it('does not query the previous file path under a new wave id when switching waves', () => {
    mockUseWaveFileContent.mockClear();
    mockUseWaveFileContent.mockReturnValue(
      contentResult({
        data: { content_type: 'text/markdown', content: '# A' },
      }),
    );

    const { rerender } = render(
      <WaveReportPage
        wave={makeWave({ id: 'wave_A' })}
        cards={[reportSlot('A body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /wave\.json/ }));

    mockUseWaveFileContent.mockClear();
    rerender(
      <WaveReportPage
        wave={makeWave({ id: 'wave_B' })}
        cards={[reportSlot('B body')]}
      />,
    );

    const badCall = mockUseWaveFileContent.mock.calls.find(
      (args) => args[0] === 'wave_B' && args[1] === 'wave.json',
    );
    expect(badCall).toBeUndefined();
  });

  it('renders the inline loading state while file content is loading', () => {
    mockWaveFileContentForPath('report.md', { isLoading: true });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    expect(screen.getByRole('status')).toHaveTextContent('Loading…');
  });

  it('renders an inline error when a non-report file fails (no fallback)', () => {
    mockWaveFileContentForPath('wave.json', {
      error: new CalmApiError(500, 'file_read_failed', 'File read failed'),
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /wave\.json/ }));

    expect(screen.getByRole('alert')).toHaveTextContent('File read failed');
  });

  it('renders a distinct inline message for unsupported content types', () => {
    mockWaveFileContentForPath('report.md', {
      data: {
        content_type: 'image/png',
        content: '...',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    expect(
      screen.getByText(/Preview unavailable for image\/png/i),
    ).toBeInTheDocument();
  });

  it('falls back to the report card body when report.md returns 404', () => {
    mockWaveFileContentForPath('report.md', {
      error: new CalmApiError(404, 'not_found', 'File not found'),
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('# Card fallback')]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Card fallback' }),
    ).toBeInTheDocument();
  });

  it('falls back to the report card body when report.md returns 500 (legacy)', () => {
    mockWaveFileContentForPath('report.md', {
      error: new CalmApiError(500, 'file_read_failed', 'File read failed'),
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Card body **fallback**')]}
      />,
    );

    expect(screen.getByText('fallback').tagName).toBe('STRONG');
  });

  it('renders the conversation drawer closed by default with the report beside it', () => {
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[specSlot(), reportSlot('Report with chat')]}
      />,
    );

    expect(screen.getByRole('button', { name: 'Open conversation drawer' })).toBeEnabled();
    expect(container.querySelector('.report-page')).not.toHaveClass(
      'report-page--conversation-open',
    );
    expect(screen.getByText('Report with chat')).toBeInTheDocument();
    expect(screen.getByLabelText('Ask the Spec Agent')).toBeInTheDocument();
  });

  it('opens the drawer without unmounting the report document', () => {
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[specSlot(), reportSlot('Report with chat')]}
      />,
    );

    expect(screen.getByText('Report with chat')).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation drawer' }),
    );

    expect(container.querySelector('.report-page')).toHaveClass(
      'report-page--conversation-open',
    );
    expect(screen.getByLabelText('Conversation')).toBeInTheDocument();
    expect(screen.getByText('Report with chat')).toBeInTheDocument();
  });

  it('opens the empty drawer and omits the composer without a spec card', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Report without spec')]}
      />,
    );

    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation drawer' }),
    );
    expect(screen.getByRole('button', { name: 'Close conversation drawer' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(
      screen.getByText('Spec Agent is unavailable for this wave.'),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText('Ask the Spec Agent'),
    ).not.toBeInTheDocument();
  });

  it('keeps the draft alive when the drawer closes and reopens', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[specSlot(), reportSlot('Report with chat')]}
      />,
    );

    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation drawer' }),
    );
    const draft = screen.getByLabelText('Ask the Spec Agent');
    fireEvent.change(draft, { target: { value: 'Persistent draft' } });

    fireEvent.click(
      screen.getByRole('button', { name: 'Close conversation drawer' }),
    );
    expect(screen.getByLabelText('Ask the Spec Agent')).toBe(draft);
    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation drawer' }),
    );
    expect(screen.getByLabelText('Ask the Spec Agent')).toHaveValue(
      'Persistent draft',
    );
  });

  it('persists the drawer state across remounts', () => {
    const props = {
      wave: makeWave(),
      cards: [specSlot(), reportSlot('Report with chat')],
    };
    const first = render(<WaveReportPage {...props} />);

    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation drawer' }),
    );
    expect(
      window.localStorage.getItem(REPORT_CONVERSATION_COLLAPSED_STORAGE_KEY),
    ).toBe('false');

    first.unmount();
    render(<WaveReportPage {...props} />);
    expect(
      screen.getByRole('button', { name: 'Close conversation drawer' }),
    ).toHaveAttribute('aria-expanded', 'true');
  });

  it('renders backlinks grouped by source wave with source block anchors', () => {
    mockUseWaveBacklinksQuery.mockReturnValue({
      data: {
        backlinks: [{
          src_wave_id: 'wave_a',
          src_wave_title: 'Alpha',
          src_block_id: 'b_a1',
          dst_block_id: 'b_here',
          label: 'First mention',
          quote: {
            before: 'context before ',
            label: 'First mention',
            after: ' context after',
            head_elided: true,
            tail_elided: true,
          },
          updated_at: 1,
        }, {
          src_wave_id: 'wave_a',
          src_wave_title: 'Alpha',
          src_block_id: 'b_a2',
          label: '',
          quote: {
            before: 'Empty label context',
            label: '',
            after: ' remains readable',
            head_elided: false,
            tail_elided: false,
          },
          updated_at: 2,
        }, {
          src_wave_id: 'wave_b',
          src_wave_title: 'Beta',
          src_block_id: 'b_b1',
          label: 'Another source',
          quote: {
            before: '',
            label: 'Another source',
            after: '',
            head_elided: false,
            tail_elided: false,
          },
          updated_at: 3,
        }],
        truncated: true,
        skipped_sources: 1,
      },
      error: null,
    } as unknown as ReturnType<typeof useWaveBacklinksQuery>);

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('Report body', {
            blocks: [
              {
                id: 'b_here',
                kind: 'prose',
                rev: 1,
                payload: { markdown: 'Report body' },
              },
            ],
          }),
        ]}
      />,
    );

    const panel = screen.getByRole('region', { name: 'Backlinks' });
    expect(within(panel).getAllByText('Alpha')).toHaveLength(2);
    expect(within(panel).getByText('Beta')).toBeInTheDocument();
    expect(
      within(panel).getByText('Some backlinks are not shown.'),
    ).toBeInTheDocument();
    expect(
      within(panel).getByText(
        'Backlinks from 1 source report could not be loaded.',
      ),
    ).toBeInTheDocument();
    expect(within(panel).getByText(/cites block b_here/)).toBeInTheDocument();
    const quotedLink = within(panel).getByRole('link', {
      name: '…context before First mention context after…',
    });
    expect(quotedLink).toHaveAttribute('href', '/wave/wave_a#b_a1');
    expect(within(quotedLink).getByText('First mention').tagName).toBe('B');
    const emptyLabelLink = within(panel).getByRole('link', {
      name: 'Empty label context remains readable',
    });
    expect(emptyLabelLink.querySelector('b')).toBeNull();
    expect(within(panel).getByRole('link', { name: 'Another source' })).toHaveAttribute(
      'href',
      '/wave/wave_b#b_b1',
    );
  });

  it('labels self-references distinctly', () => {
    mockUseWaveBacklinksQuery.mockReturnValue({
      data: {
        backlinks: [{
          src_wave_id: 'wave_1',
          src_wave_title: 'Spec wave',
          src_block_id: 'b_source',
          dst_block_id: 'b_target',
          label: 'Self citation',
          updated_at: 1,
        }],
        truncated: false,
        skipped_sources: 0,
      },
      error: null,
    } as unknown as ReturnType<typeof useWaveBacklinksQuery>);

    render(<WaveReportPage wave={makeWave()} cards={[reportSlot('Report body')]} />);
    expect(screen.getByText('This wave (self-reference)')).toBeInTheDocument();
  });

  it('does not promise a block target for a flat v1 report', () => {
    mockUseWaveBacklinksQuery.mockReturnValue({
      data: {
        backlinks: [{
          src_wave_id: 'wave_a',
          src_wave_title: 'Alpha',
          src_block_id: 'b_a1',
          dst_block_id: 'b_here',
          label: 'Legacy target',
          updated_at: 1,
        }],
        truncated: false,
        skipped_sources: 0,
      },
      error: null,
    } as unknown as ReturnType<typeof useWaveBacklinksQuery>);

    render(
      <WaveReportPage wave={makeWave()} cards={[reportSlot('Report body')]} />,
    );
    expect(screen.getByRole('link', { name: 'Legacy target' })).toBeVisible();
    expect(screen.queryByText(/cites block b_here/)).toBeNull();
  });

  it('surfaces backlink loading errors inline', () => {
    mockUseWaveBacklinksQuery.mockReturnValue({
      data: undefined,
      error: new Error('server unavailable'),
    } as unknown as ReturnType<typeof useWaveBacklinksQuery>);

    render(<WaveReportPage wave={makeWave()} cards={[reportSlot('Report body')]} />);
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Could not load backlinks: server unavailable',
    );
  });

  it('renders a neige link in a flat-body report as an in-app link', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('[Target](neige://wave/wave_2#b_cafe)')]}
      />,
    );
    expect(screen.getByRole('link', { name: 'Target' })).toHaveAttribute(
      'href',
      '/wave/wave_2#b_cafe',
    );
  });

  it('does not scroll again when only the blocks array identity changes', () => {
    window.history.replaceState(null, '', '#b_present');
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      value: () => {},
    });
    const scrollIntoView = vi
      .spyOn(Element.prototype, 'scrollIntoView')
      .mockImplementation(
        () => {},
      );
    const block = {
      id: 'b_present',
      kind: 'prose' as const,
      rev: 1,
      payload: { markdown: 'Present block' },
    };
    const { rerender } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('fallback', { blocks: [block] })]}
      />,
    );
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    rerender(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('fallback', { blocks: [{ ...block }] })]}
      />,
    );
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
  });

  it('scrolls when the hash target arrives after the report starts loading', () => {
    window.history.replaceState(null, '', '#b_late');
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      value: () => {},
    });
    const scrollIntoView = vi
      .spyOn(Element.prototype, 'scrollIntoView')
      .mockImplementation(() => {});
    const { rerender } = render(
      <WaveReportPage wave={makeWave()} cards={[reportSlot('Loading report')]} />,
    );
    expect(scrollIntoView).not.toHaveBeenCalled();

    rerender(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot('fallback', {
            blocks: [
              {
                id: 'b_late',
                kind: 'prose',
                rev: 1,
                payload: { markdown: 'Late block' },
              },
            ],
          }),
        ]}
      />,
    );
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
  });

  it('keeps the backlinks section visible with an empty state', () => {
    render(
      <WaveReportPage wave={makeWave()} cards={[reportSlot('Report body')]} />,
    );
    const backlinks = screen.getByRole('region', { name: 'Backlinks' });
    expect(within(backlinks).getByText('No backlinks yet.')).toBeInTheDocument();
  });

  it('renders normally when the hash names a missing block', () => {
    window.history.replaceState(null, '', '#b_missing');
    expect(() =>
      render(
        <WaveReportPage
          wave={makeWave()}
          cards={[
            reportSlot('fallback', {
              blocks: [
                {
                  id: 'b_present',
                  kind: 'prose',
                  rev: 1,
                  payload: { markdown: 'Present block' },
                },
              ],
            }),
          ]}
        />,
      ),
    ).not.toThrow();
    expect(screen.getByText('Present block')).toBeInTheDocument();
    expect(document.getElementById('b_present')).not.toHaveClass(
      'report-block--highlight',
    );
  });
});
