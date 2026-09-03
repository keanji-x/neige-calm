import {
  fireEvent,
  render as testingLibraryRender,
  screen,
  waitFor,
  within,
} from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReactElement, ReactNode } from 'react';
import { initialBody, splitInitialBody } from './report-blocks/kernel-initial-body.ts';
import { TrackReportPage } from './TrackReportPage';
import {
  useOverlaysByKindQuery,
  useTrackBacklinksQuery,
  useTrackFileContent,
  useTrackFileList,
  useTrackReportQuery,
} from '../api/queries';
import { CalmApiError, type TrackFsContent, type TrackFsEntry } from '../api/calm';
import * as api from '../api/calm';
import type { Track, TrackCardSlot } from '../types';
import type { TrackReportCardData } from '../cards/builtins/track-report';
import { usePlannerChatHistory } from './usePlannerChatHistory';
import { usePlannerCurrentRun } from './usePlannerCurrentRun';

vi.mock('../api/queries', () => ({
  useOverlaysByKindQuery: vi.fn(),
  useTrackBacklinksQuery: vi.fn(),
  useTrackFileList: vi.fn(),
  useTrackFileContent: vi.fn(),
  useTrackReportQuery: vi.fn(),
}));

vi.mock('./usePlannerChatHistory', () => ({
  usePlannerChatHistory: vi.fn(),
}));

vi.mock('./usePlannerCurrentRun', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./usePlannerCurrentRun')>();
  return { ...actual, usePlannerCurrentRun: vi.fn() };
});

vi.mock('@tanstack/react-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@tanstack/react-router')>();
  return {
    ...actual,
    Link: ({
      params,
      hash,
      children,
    }: {
      params: { trackId: string };
      hash?: string;
      children: React.ReactNode;
    }) => (
      <a href={`/track/${params.trackId}${hash ? `#${hash}` : ''}`}>{children}</a>
    ),
    useRouterState: <T,>({
      select,
    }: {
      select: (state: { location: { hash: string } }) => T;
    }) => select({ location: { hash: window.location.hash.slice(1) } }),
  };
});

// The planner-conversation panel's status dot reads `useCardOverlay`, which is
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

function render(ui: ReactElement) {
  const queryClient = new QueryClient({
    defaultOptions: { mutations: { retry: false }, queries: { retry: false } },
  });
  return testingLibraryRender(ui, {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    ),
  });
}

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

const mockUseTrackFileList = vi.mocked(useTrackFileList);
const mockUseTrackFileContent = vi.mocked(useTrackFileContent);
const mockUseTrackReportQuery = vi.mocked(useTrackReportQuery);
const mockUseTrackBacklinksQuery = vi.mocked(useTrackBacklinksQuery);
const mockUseOverlaysByKindQuery = vi.mocked(useOverlaysByKindQuery);
const mockUsePlannerChatHistory = vi.mocked(usePlannerChatHistory);
const mockUsePlannerCurrentRun = vi.mocked(usePlannerCurrentRun);

const REPORT_RAIL_COLLAPSED_STORAGE_KEY = 'calm:report-rail:collapsed';
const REPORT_OUTLINE_COLLAPSED_STORAGE_KEY =
  'calm:report-rail:outline:collapsed';
const REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY =
  'calm:report-rail:backlinks:collapsed';
const REPORT_FILES_COLLAPSED_STORAGE_KEY = 'calm:report-rail:files:collapsed';
const REPORT_CONVERSATION_COLLAPSED_STORAGE_KEY =
  'calm:report-conversation:collapsed';

function makeTrack(overrides: Partial<Track> = {}): Track {
  return {
    id: 'track_1',
    areaId: 'area_1',
    title: 'Planner track',
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
  overrides: Partial<TrackReportCardData> & { sort?: number } = {},
): TrackCardSlot {
  const { sort, ...cardOverrides } = overrides;
  const card: TrackReportCardData = {
    type: 'track-report',
    id: 'report_1',
    summary: '',
    body,
    docRev: 0,
  };
  return {
    kind: 'card',
    card: { ...card, ...cardOverrides },
    sort,
    deletable: false,
  };
}

function plannerSlot(id = 'card_planner_1'): TrackCardSlot {
  return {
    kind: 'card',
    card: {
      type: 'planner',
      id,
      goal: 'Answer follow-up questions',
    },
    sort: 0,
    deletable: false,
  };
}

function contentResult(
  value: Partial<ReturnType<typeof useTrackFileContent>> = {},
) {
  return {
    data: undefined,
    error: null,
    isLoading: false,
    ...value,
  } as unknown as ReturnType<typeof useTrackFileContent>;
}

function mockTrackFileContentForPath(
  path: string,
  value: Partial<ReturnType<typeof useTrackFileContent>>,
) {
  mockUseTrackFileContent.mockImplementation((_, requestedPath) => {
    if (requestedPath === path) {
      return contentResult(value);
    }
    return contentResult();
  });
}

function mockTrackFileContents(contents: Record<string, TrackFsContent>) {
  mockUseTrackFileContent.mockImplementation((_, requestedPath) => {
    const data = requestedPath ? contents[requestedPath] : undefined;
    return contentResult(data ? { data } : undefined);
  });
}

function mockTrackFileLists(lists: Record<string, TrackFsEntry[]>) {
  mockUseTrackFileList.mockImplementation((_, requestedPath = '') => {
    const path = requestedPath ?? '';
    return {
      data: lists[path],
      error: null,
      isLoading: false,
    } as unknown as ReturnType<typeof useTrackFileList>;
  });
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  delete (Element.prototype as Partial<Element>).scrollIntoView;
  window.localStorage.clear();
  window.history.replaceState(null, '', window.location.pathname);
});

describe('TrackReportPage', () => {
  beforeEach(() => {
    mockUseTrackReportQuery.mockReturnValue({
      data: undefined,
      refetch: vi.fn(async () => ({ data: undefined })),
    } as unknown as ReturnType<typeof useTrackReportQuery>);
    mockUsePlannerChatHistory.mockReturnValue({
      entries: [],
      initialLoading: false,
      hasEarlier: false,
      loadEarlierPending: false,
      loadEarlier: vi.fn(async () => {}),
      addEcho: vi.fn(),
      addSystemNote: vi.fn(),
    });
    mockUsePlannerCurrentRun.mockReturnValue({
      cardId: 'card_planner_1',
      rawState: 'Idle',
      fsm: 'Idle',
      phase: 'idle',
      working: false,
      stopping: false,
      latestTool: { toolLabel: null, toolStatus: null },
      resetPending: false,
      resetError: null,
      reset: vi.fn(async () => {}),
      submit: vi.fn(async () => {}),
      submitPending: false,
      submitError: null,
      submitDormant: false,
      stop: vi.fn(async () => false),
      stopPending: false,
      stopError: null,
    });
    mockUseTrackBacklinksQuery.mockReturnValue({
      data: { backlinks: [], truncated: false, skipped_sources: 0 },
      error: null,
    } as unknown as ReturnType<typeof useTrackBacklinksQuery>);
    mockUseOverlaysByKindQuery.mockReturnValue({
      data: [],
    } as unknown as ReturnType<typeof useOverlaysByKindQuery>);
    const files: TrackFsEntry[] = [
      { name: 'report.md', kind: 'file' },
      { name: 'track.json', kind: 'file' },
    ];
    mockUseTrackFileList.mockReturnValue({
      data: files,
      error: null,
      isLoading: false,
    } as unknown as ReturnType<typeof useTrackFileList>);
    mockUseTrackFileContent.mockImplementation((_, requestedPath) => {
      if (requestedPath === 'report.md') {
        return contentResult({
          error: new CalmApiError(404, 'not_found', 'File not found'),
        });
      }
      return contentResult();
    });
  });

  it('renders the empty state when there is no report card and report.md is missing', () => {
    render(<TrackReportPage track={makeTrack()} cards={[]} />);

    expect(
      screen.getByText(
        'Report not ready. The planner agent has not produced a report yet.',
      ),
    ).toBeInTheDocument();
  });

  it('skips the report.md fetch when there is no report card', () => {
    mockUseTrackFileContent.mockClear();

    render(<TrackReportPage track={makeTrack()} cards={[]} />);

    const reportMdCall = mockUseTrackFileContent.mock.calls.find(
      (args) => args[1] === 'report.md',
    );
    expect(reportMdCall).toBeUndefined();
  });

  it('renders a non-report file even when the track has no report card', async () => {
    mockUseTrackFileContent.mockImplementation((_, requestedPath) => {
      if (requestedPath === 'track.json') {
        return contentResult({
          data: { content_type: 'text/plain', content: 'plain text' },
        });
      }
      return contentResult();
    });

    render(<TrackReportPage track={makeTrack()} cards={[]} />);

    fireEvent.click(screen.getByRole('treeitem', { name: /track\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      'plain text',
    );
  });

  it('renders the report in its own focusable scroll root', () => {
    const { container } = render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('The **answer** is ready.')]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Planner track' }),
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

  it('shows only user turns newest-first and opens the drawer at a selected turn', async () => {
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      value: vi.fn(),
    });
    mockUsePlannerChatHistory.mockReturnValue({
      entries: [
        { id: 1, atMs: 1, kind: 'user', text: 'Old instruction' },
        { id: 2, atMs: 2, kind: 'agent', text: 'Agent reply' },
        { id: 3, atMs: 3, kind: 'system', text: 'System note' },
        { id: 4, atMs: 4, kind: 'user', text: 'New instruction' },
      ],
      initialLoading: false,
      hasEarlier: false,
      loadEarlierPending: false,
      loadEarlier: vi.fn(async () => {}),
      addEcho: vi.fn(),
      addSystemNote: vi.fn(),
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report'), plannerSlot()]}
      />,
    );

    const panel = screen.getByLabelText('Recent conversation activity');
    expect(
      within(panel).getAllByRole('button').map((row) => row.textContent),
    ).toEqual(['New instruction', 'Old instruction']);
    expect(within(panel).queryByText('Agent reply')).not.toBeInTheDocument();
    expect(within(panel).queryByText('System note')).not.toBeInTheDocument();

    fireEvent.click(within(panel).getByRole('button', { name: 'Old instruction' }));
    expect(screen.getByLabelText('Conversation drawer')).toHaveClass(
      'report-conversation-drawer--open',
    );
    expect(panel).toHaveClass('hide');
    expect(panel).toHaveAttribute('aria-hidden', 'true');
    await waitFor(() => {
      expect(Element.prototype.scrollIntoView).toHaveBeenCalled();
    });

    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    fireEvent.click(screen.getByRole('button', { name: 'New instruction' }));
    await new Promise((resolve) => window.setTimeout(resolve, 60));
    expect(Element.prototype.scrollIntoView).toHaveBeenCalledTimes(2);
  });

  it('does not repeat target scrolling when reopened without a target', async () => {
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      value: vi.fn(),
    });
    mockUsePlannerChatHistory.mockReturnValue({
      entries: [
        { id: 1, atMs: 1, kind: 'user', text: 'Pinned instruction' },
      ],
      initialLoading: false,
      hasEarlier: false,
      loadEarlierPending: false,
      loadEarlier: vi.fn(async () => {}),
      addEcho: vi.fn(),
      addSystemNote: vi.fn(),
    });

    const { rerender } = render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report'), plannerSlot()]}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Pinned instruction' }));
    await waitFor(() => {
      expect(Element.prototype.scrollIntoView).toHaveBeenCalledTimes(1);
    });
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));

    mockUsePlannerChatHistory.mockReturnValue({
      entries: [],
      initialLoading: false,
      hasEarlier: false,
      loadEarlierPending: false,
      loadEarlier: vi.fn(async () => {}),
      addEcho: vi.fn(),
      addSystemNote: vi.fn(),
    });
    rerender(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report'), plannerSlot()]}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Open conversation' }));
    await new Promise((resolve) => window.setTimeout(resolve, 60));
    expect(Element.prototype.scrollIntoView).toHaveBeenCalledTimes(1);
  });

  it('does not claim the conversation is empty before initial history loads', () => {
    mockUsePlannerChatHistory.mockReturnValue({
      entries: [],
      initialLoading: true,
      hasEarlier: false,
      loadEarlierPending: false,
      loadEarlier: vi.fn(async () => {}),
      addEcho: vi.fn(),
      addSystemNote: vi.fn(),
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report'), plannerSlot()]}
      />,
    );

    expect(screen.getByText('Loading conversation activity…'))
      .toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Open conversation' }))
      .not.toBeInTheDocument();
  });

  it('caps activity focus stops and discloses omitted earlier turns', () => {
    mockUsePlannerChatHistory.mockReturnValue({
      entries: Array.from({ length: 15 }, (_, index) => ({
        id: index + 1,
        atMs: index + 1,
        kind: 'user' as const,
        text: `Instruction ${index + 1}`,
      })),
      initialLoading: false,
      hasEarlier: false,
      loadEarlierPending: false,
      loadEarlier: vi.fn(async () => {}),
      addEcho: vi.fn(),
      addSystemNote: vi.fn(),
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report'), plannerSlot()]}
      />,
    );

    const panel = screen.getByLabelText('Recent conversation activity');
    expect(within(panel).getAllByRole('button')).toHaveLength(12);
    expect(within(panel).getByText('3 earlier turns')).toBeInTheDocument();
    expect(within(panel).queryByText('Instruction 3')).not.toBeInTheDocument();
  });

  it('does not claim an exact omitted count while earlier pages remain', () => {
    mockUsePlannerChatHistory.mockReturnValue({
      entries: Array.from({ length: 15 }, (_, index) => ({
        id: index + 1,
        atMs: index + 1,
        kind: 'user' as const,
        text: `Instruction ${index + 1}`,
      })),
      initialLoading: false,
      hasEarlier: true,
      loadEarlierPending: false,
      loadEarlier: vi.fn(async () => {}),
      addEcho: vi.fn(),
      addSystemNote: vi.fn(),
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report'), plannerSlot()]}
      />,
    );

    const panel = screen.getByLabelText('Recent conversation activity');
    expect(within(panel).getAllByRole('button')).toHaveLength(12);
    expect(within(panel).getByText('At least 3 earlier turns'))
      .toBeInTheDocument();
    expect(within(panel).queryByText('3 earlier turns')).not.toBeInTheDocument();
  });

  it('distinguishes both activity empty states', () => {
    const { rerender } = render(
      <TrackReportPage track={makeTrack()} cards={[reportSlot('Report')]} />,
    );
    expect(screen.getByText('This track has no Planner Agent.')).toBeInTheDocument();

    rerender(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report'), plannerSlot()]}
      />,
    );
    expect(screen.getByRole('button', { name: 'Open conversation' }))
      .toBeInTheDocument();
    expect(screen.queryByText('This track has no Planner Agent.'))
      .not.toBeInTheDocument();
  });

  it('puts the working indicator on only the newest user turn', () => {
    mockUsePlannerChatHistory.mockReturnValue({
      entries: [
        { id: 1, atMs: 1, kind: 'user', text: 'Old instruction' },
        { id: 2, atMs: 2, kind: 'user', text: 'New instruction' },
      ],
      initialLoading: false,
      hasEarlier: false,
      loadEarlierPending: false,
      loadEarlier: vi.fn(async () => {}),
      addEcho: vi.fn(),
      addSystemNote: vi.fn(),
    });
    mockUsePlannerCurrentRun.mockReturnValue({
      ...mockUsePlannerCurrentRun(undefined),
      working: true,
      fsm: 'Working',
      phase: 'turn_running',
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report'), plannerSlot()]}
      />,
    );

    const panel = screen.getByLabelText('Recent conversation activity');
    const rows = within(panel).getAllByRole('button');
    expect(within(rows[0]).getByLabelText('Planner Agent is working'))
      .toHaveClass('busy');
    expect(within(rows[1]).queryByLabelText('Planner Agent is working'))
      .not.toBeInTheDocument();
  });

  it('renders derived prose blocks when present', () => {
    render(
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage
        track={makeTrack()}
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

  it('preserves heading text and spaces around inline markdown', () => {
    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot(
          '## Latency `p99` **regression** via [R8](https://example.com/r8)',
        )]}
      />,
    );

    const heading = screen.getByRole('heading', {
      level: 2,
      name: 'Latency p99 regression via R8',
    });
    expect(heading.textContent).toBe('Latency p99 regression via R8');
    expect(within(heading).getByText('p99').tagName).toBe('CODE');
    expect(within(heading).getByText('regression').tagName).toBe('STRONG');
    expect(within(heading).getByRole('link', { name: 'R8' })).toBeInTheDocument();
  });

  it('scrolls a multi-H2 outline link to its matching heading', async () => {
    Object.defineProperty(Element.prototype, 'scrollIntoView', {
      configurable: true,
      value: () => {},
    });
    const scrollIntoView = vi.spyOn(Element.prototype, 'scrollIntoView');
    render(
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage track={makeTrack()} cards={[reportSlot('## Flat heading')]} />,
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
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage
        track={makeTrack()}
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

  it('shows the version wall when a report API block is malformed', () => {
    mockUseTrackReportQuery.mockReturnValue({
      data: {
        schemaVersion: 1,
        docRev: 1,
        summary: '',
        body: '',
        blocks: [{ id: 'b_bad', kind: 'task', rev: 1, payload: { key: 'missing-fields' } }],
        taskDiagnostics: [],
      },
      refetch: vi.fn(),
    } as unknown as ReturnType<typeof useTrackReportQuery>);

    render(<TrackReportPage track={makeTrack()} cards={[reportSlot('fallback')]} />);

    expect(screen.getByRole('alert')).toHaveTextContent('版本不支持，请刷新');
    expect(screen.queryByText('fallback')).not.toBeInTheDocument();
  });

  it('wires projected task UI and every task action through the page assembly', async () => {
    const refetch = vi.fn(async () => ({ data: undefined }));
    mockUseTrackReportQuery.mockReturnValue({
      data: {
        schemaVersion: 1,
        docRev: 9,
        summary: '',
        body: '',
        blocks: [
          {
            id: 'b_task', kind: 'task', rev: 3,
            payload: {
              key: 'ship', kind: 'codex', goal: 'Ship the fix', ready: true,
              declared_by: 'spec', released_by_user: false,
            },
          },
          {
            id: 'b_second', kind: 'task', rev: 5,
            payload: {
              key: 'verify', kind: 'codex', goal: 'Verify the join', ready: false,
              declared_by: 'spec', released_by_user: false,
            },
          },
          {
            id: 'b_tombstone', kind: 'task', rev: 4,
            payload: {
              key: 'declined', tombstone: { reason: 'not now' },
              declared_by: 'spec', tombstoned_by: 'user',
            },
          },
        ],
        taskDiagnostics: [
          {
            blockId: 'b_second', key: 'verify', schedulable: false, status: null,
            gateResult: { passed: false, failing_step: 'lint' },
            workerCardId: 'card_worker_second', diagnostics: [],
          },
          {
            blockId: 'b_task', key: 'ship', schedulable: false, status: 'pending',
            gateResult: { passed: true }, workerCardId: 'card_worker_ship',
            diagnostics: [
            {
              code: 'declare_and_wait', messageArgs: {}, relatedBlockIds: [],
              relatedTrackId: undefined,
              path: 'released_by_user', message: 'compat', action: 'release_task',
            },
            {
              code: 'context_stale_reference', messageArgs: {},
              relatedBlockIds: ['b_cafe'], relatedTrackId: 'track_2',
              path: 'refs', message: 'compat', action: 'relink_reference',
            },
          ],
          },
        ],
      },
      refetch,
    } as unknown as ReturnType<typeof useTrackReportQuery>);
    const updateBlock = vi.spyOn(api, 'updateTrackReportBlock').mockResolvedValue({});
    const deleteBlock = vi.spyOn(api, 'deleteTrackReportBlock').mockResolvedValue({});
    const updateTrack = vi.spyOn(api, 'updateTrack').mockResolvedValue({} as never);
    const confirm = vi.spyOn(window, 'confirm')
      .mockReturnValueOnce(false)
      .mockReturnValueOnce(true);

    mockTrackFileContents({
      'report.md': { content_type: 'text/markdown', content: '# Projected report' },
    });

    render(<TrackReportPage track={makeTrack()} cards={[reportSlot('fallback')]} />);

    const task = screen.getByRole('region', { name: 'Task ship' });
    expect(within(task).getByText('Waiting to start')).toBeInTheDocument();
    expect(within(task).getByText('Checks: passed')).toBeInTheDocument();
    const alerts = within(task).getAllByRole('alert');
    expect(alerts).toHaveLength(2);
    expect(alerts[0]).toHaveTextContent('AI-proposed tasks in this track wait for you');
    expect(alerts[1]).toHaveTextContent(
      'saved reference context no longer matches its frozen closure',
    );
    expect(within(task).getByRole('link', { name: 'b_cafe' })).toHaveAttribute(
      'href', '/track/track_2#b_cafe',
    );
    expect(within(task).getByRole('link', { name: 'referenced track' })).toHaveAttribute(
      'href', '/track/track_2',
    );
    expect(within(task).getByRole('link', { name: 'Open worker output' })).toHaveAttribute(
      'href', '/track/track_1#card_worker_ship',
    );
    const secondTask = screen.getByRole('region', { name: 'Task verify' });
    expect(within(secondTask).getByText('Not queued')).toBeInTheDocument();
    expect(within(secondTask).getByText('Checks: failed at lint')).toBeInTheDocument();
    expect(within(secondTask).getByRole('link', { name: 'Open worker output' }))
      .toHaveAttribute('href', '/track/track_1#card_worker_second');

    fireEvent.click(within(task).getByRole('button', { name: 'Allow this task' }));
    await waitFor(() => expect(updateBlock).toHaveBeenCalledWith('track_1', 'b_task', {
      kind: 'task', ifBlockRev: 3,
      payload: expect.objectContaining({ key: 'ship', released_by_user: true }),
    }));
    fireEvent.click(within(task).getByRole('button', { name: 'Remove task' }));
    await waitFor(() => expect(deleteBlock).toHaveBeenCalledWith('track_1', 'b_task', 3));
    expect(confirm).toHaveBeenLastCalledWith(
      'Should future Planner tasks wait for your approval?\n\n“OK” = yes; “Cancel” = remove only this task.',
    );
    expect(updateTrack).not.toHaveBeenCalled();
    fireEvent.click(within(task).getByRole('button', { name: 'Restore automatic AI tasks' }));
    await waitFor(() => expect(updateTrack).toHaveBeenCalledWith('track_1', {
      automation_policy: 'auto-declare',
    }));
    fireEvent.click(within(secondTask).getByRole('button', { name: 'Remove task' }));
    await waitFor(() => expect(deleteBlock).toHaveBeenCalledWith('track_1', 'b_second', 5));
    expect(confirm).toHaveBeenCalledTimes(2);
    expect(confirm).toHaveBeenLastCalledWith(
      'Should future Planner tasks wait for your approval?\n\n“OK” = yes; “Cancel” = remove only this task.',
    );
    await waitFor(() => expect(updateTrack).toHaveBeenCalledWith('track_1', {
      automation_policy: 'declare-and-wait',
    }));

    const tombstone = screen.getByRole('region', { name: 'Do not do declined' });
    fireEvent.click(within(tombstone).getByRole('button', { name: 'Allow this key again' }));
    await waitFor(() => expect(deleteBlock).toHaveBeenCalledWith('track_1', 'b_tombstone', 4));
    fireEvent.click(within(tombstone).getByRole('button', { name: 'Restore automatic AI tasks' }));
    await waitFor(() => expect(updateTrack).toHaveBeenCalledTimes(3));
    expect(refetch).toHaveBeenCalledTimes(6);

    deleteBlock.mockRejectedValueOnce(
      new CalmApiError(409, 'stale_block_rev', 'The task card changed; refresh and try again.'),
    );
    fireEvent.click(within(tombstone).getByRole('button', { name: 'Allow this key again' }));
    expect(await within(tombstone).findByRole('alert')).toHaveTextContent(
      'Task action failed: The task card changed; refresh and try again.',
    );
  });

  it('shows the duplicate banner and renders the lowest-sort report', () => {
    render(
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    expect(
      screen.getByRole('tree', { name: 'Track files' }),
    ).toBeInTheDocument();
    expect(screen.getByRole('treeitem', { name: /report\.md/ })).toBeTruthy();
    expect(
      screen.queryByText('Track files appear here. (Wired in PR-B.)'),
    ).not.toBeInTheDocument();
  });

  it('toggles dotfile rows in the Files rail without hiding siblings', async () => {
    mockTrackFileLists({
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
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
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
    const fileTree = screen.getByRole('tree', { name: 'Track files' });
    expect(
      showAll.compareDocumentPosition(fileTree) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).not.toBe(0);
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

  it('collapses and expands the report rail from its edge controls', () => {
    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const rail = screen.getByLabelText('Report context');
    const collapseToggle = screen.getByRole('button', {
      name: 'Collapse report rail',
    });

    expect(rail).not.toHaveClass('report-rail--collapsed');
    expect(screen.getByRole('tree', { name: 'Track files' })).toBeInTheDocument();

    fireEvent.click(collapseToggle);

    const expandToggle = screen.getByRole('button', {
      name: 'Expand report rail',
    });
    expect(expandToggle).toHaveAttribute('aria-controls', 'report-context-rail');
    expect(expandToggle).toHaveAttribute('aria-expanded', 'false');
    expect(rail).toHaveClass('report-rail--collapsed');
    expect(screen.queryByRole('tree', { name: 'Track files' })).toBeNull();
    expect(window.localStorage.getItem(REPORT_RAIL_COLLAPSED_STORAGE_KEY))
      .toBe('true');

    fireEvent.click(expandToggle);

    expect(screen.getByRole('button', { name: 'Collapse report rail' }))
      .toHaveAttribute('aria-expanded', 'true');
    expect(rail).not.toHaveClass('report-rail--collapsed');
    expect(screen.getByRole('tree', { name: 'Track files' })).toBeInTheDocument();
    expect(window.localStorage.getItem(REPORT_RAIL_COLLAPSED_STORAGE_KEY))
      .toBe('false');
  });

  it('switches focus between the mirrored rail edge controls', () => {
    render(
      <TrackReportPage
        track={makeTrack()}
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
    expect(document.activeElement).toBe(expandToggle);

    fireEvent.click(expandToggle);

    const collapseToggle = screen.getByRole('button', {
      name: 'Collapse report rail',
    });
    expect(document.activeElement).toBe(collapseToggle);
  });

  it('persists the collapsed Files rail across remounts', () => {
    const props = {
      track: makeTrack(),
      cards: [reportSlot('Files rail body')],
    };
    const { unmount } = render(<TrackReportPage {...props} />);

    fireEvent.click(screen.getByRole('button', { name: 'Collapse report rail' }));
    unmount();
    render(<TrackReportPage {...props} />);

    expect(screen.getByRole('button', { name: 'Expand report rail' }))
      .toHaveAttribute('aria-controls', 'report-context-rail');
    expect(screen.getByLabelText('Report context'))
      .toHaveClass('report-rail--collapsed');
    expect(screen.queryByRole('tree', { name: 'Track files' })).toBeNull();
  });

  it('persists each collapsible rail section independently', () => {
    const props = {
      track: makeTrack(),
      cards: [reportSlot('Rail body')],
    };
    const { unmount } = render(<TrackReportPage {...props} />);

    fireEvent.click(screen.getByRole('button', { name: 'Outline' }));
    fireEvent.click(screen.getByRole('button', { name: 'Backlinks' }));
    fireEvent.click(screen.getByRole('button', { name: 'Files' }));

    expect(screen.queryByRole('button', { name: 'Show all' })).toBeNull();
    expect(mockUseTrackFileList).toHaveBeenCalledWith(
      'track_1',
      '',
      { enabled: false },
    );

    expect(
      window.localStorage.getItem(REPORT_BACKLINKS_COLLAPSED_STORAGE_KEY),
    ).toBe('true');
    expect(
      window.localStorage.getItem(REPORT_OUTLINE_COLLAPSED_STORAGE_KEY),
    ).toBe('true');
    expect(
      window.localStorage.getItem(REPORT_FILES_COLLAPSED_STORAGE_KEY),
    ).toBe('true');

    unmount();
    render(<TrackReportPage {...props} />);
    expect(screen.getByRole('button', { name: 'Outline' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByRole('button', { name: 'Backlinks' }))
      .toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByRole('button', { name: 'Files' }))
      .toHaveAttribute('aria-expanded', 'false');
  });

  it('defaults the report rail to collapsed at the narrow-screen breakpoint', () => {
    vi.stubGlobal(
      'matchMedia',
      vi.fn(() => ({ matches: true })) as unknown as typeof window.matchMedia,
    );

    render(
      <TrackReportPage track={makeTrack()} cards={[reportSlot('Narrow body')]} />,
    );

    expect(screen.getByLabelText('Report context'))
      .toHaveClass('report-rail--collapsed');
    expect(screen.getByRole('button', { name: 'Expand report rail' }))
      .toHaveAttribute('aria-controls', 'report-context-rail');
  });

  it('renders the three rail sections in order with Event line removed', () => {
    const { container } = render(
      <TrackReportPage
        track={makeTrack()}
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
    expect(page?.children[1]).toHaveClass('report-rail-close');
    expect(page?.children[2]).toHaveClass('report-center');
    expect(page?.children[2]?.firstElementChild).toHaveClass('report-rail-open');
    expect(page?.lastElementChild).toHaveClass('report-conversation-drawer');
  });

  it('defaults the main column to report.md content', () => {
    mockTrackFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    expect(
      screen.getByRole('treeitem', { name: /report\.md/ }),
    ).toHaveAttribute('aria-selected', 'true');
    expect(
      screen.getByRole('heading', { level: 1, name: 'Hi' }),
    ).toBeInTheDocument();
    expect(mockUseTrackFileContent).toHaveBeenCalledWith('track_1', 'report.md', {
      enabled: true,
    });
  });

  it('switches the main column to another selected file', async () => {
    mockTrackFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'track.json': {
        content_type: 'application/json',
        content: '{"ok":true}',
      },
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Hi' }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole('treeitem', { name: /track\.json/ }));

    expect(
      screen.queryByRole('heading', { level: 1, name: 'Hi' }),
    ).not.toBeInTheDocument();
    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"ok":true}',
    );
  });

  it('switches back to report.md from the Files tree', async () => {
    mockTrackFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'track.json': {
        content_type: 'application/json',
        content: '{"ok":true}',
      },
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /track\.json/ }));
    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"ok":true}',
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));

    expect(
      await screen.findByRole('heading', { level: 1, name: 'Hi' }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the cards/index.json track fs viewer', async () => {
    mockTrackFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'index.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
            kind: 'track-report',
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(
      await screen.findByRole('heading', { name: 'Cards in this track (2)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('codex')).toHaveClass(
      'track-fs-viewer-card-title',
    );
    expect(screen.getByText('worker')).toHaveClass('track-fs-viewer-chip');
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the track.json track fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockTrackFileContents({
      'report.md': {
        content_type: 'text/markdown',
        content: '# Hi',
      },
      'track.json': {
        content_type: 'application/json',
        content: JSON.stringify({
          title: 'Track fs registry',
          id: 'track_json_1',
          area_id: 'area_json_1',
          lifecycle: 'working',
          cwd: '/repo/neige-calm',
          template_id: null,
          plugin_scope: null,
          template_input: null,
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /track\.json/ }));

    expect(
      await screen.findByRole('heading', { name: 'Track fs registry' }),
    ).toHaveClass('track-fs-viewer-primary');
    expect(screen.getByText('track_json_1')).toHaveClass('track-fs-viewer-mono');
    expect(screen.getByText('Pinned 5m ago')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the cards/<id>/.meta.json track fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockTrackFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_meta/', kind: 'dir' }],
      'cards/card_meta': [{ name: '.meta.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
          role: 'planner',
          sort: 5,
          deletable: false,
          created_at: new Date('2026-06-10T10:00:00Z').getTime(),
          updated_at: new Date('2026-06-10T11:55:00Z').getTime(),
        }),
      },
    });

    render(
      <TrackReportPage
        track={makeTrack()}
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
      'track-fs-viewer-primary',
    );
    expect(screen.getByText('planner')).toHaveClass('track-fs-viewer-chip');
    expect(screen.getByText('deletable: no')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the cards/<id>/events.json track fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockTrackFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_events/', kind: 'dir' }],
      'cards/card_events': [{ name: 'events.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
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
      'track-fs-viewer-primary',
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

  it('renders the cards/<id>/runtime.json track fs viewer', async () => {
    mockTrackFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_runtime/', kind: 'dir' }],
      'cards/card_runtime': [{ name: 'runtime.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
          source: 'track-dispatcher',
          thread_status: 'queued',
        }),
      },
    });

    render(
      <TrackReportPage
        track={makeTrack()}
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
      'track-fs-viewer-primary',
    );
    expect(screen.getByText('runtime_page_1')).toHaveClass(
      'track-fs-viewer-mono',
    );
    expect(screen.getByText('turn_pending')).toHaveAttribute(
      'data-tone',
      'warning',
    );
    expect(screen.getByText('claude', { selector: '.track-fs-viewer-chip' }))
      .toBeInTheDocument();
    expect(screen.getByText('terminal_page_1')).toHaveClass(
      'track-fs-viewer-mono',
    );
    expect(screen.getByText('thread_page_1')).toHaveClass(
      'track-fs-viewer-mono',
    );
    expect(screen.getByText('session_page_1')).toHaveClass(
      'track-fs-viewer-mono',
    );
    expect(screen.getByText('track-dispatcher')).toHaveClass(
      'track-fs-viewer-mono',
    );
    expect(screen.getByText('queued')).toHaveClass('track-fs-viewer-mono');
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the runs/index.json track fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockTrackFileLists({
      '': [
        { name: 'report.md', kind: 'file' },
        { name: 'runs/', kind: 'dir', size: 1 },
      ],
      runs: [{ name: 'index.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /runs\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(
      await screen.findByRole('heading', { name: 'Runs in this track (1)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('run_codex_1')).toHaveClass(
      'track-fs-viewer-mono',
    );
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('renders the runs/<key>.json track fs viewer', async () => {
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
    mockTrackFileLists({
      '': [
        { name: 'report.md', kind: 'file' },
        { name: 'runs/', kind: 'dir', size: 1 },
      ],
      runs: [{ name: 'run_terminal_1.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /runs\// }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /run_terminal_1\.json/ }),
    );

    expect(
      await screen.findByRole('heading', { name: 'terminal' }),
    ).toHaveClass('track-fs-viewer-primary');
    expect(screen.getByText('run_terminal_1')).toHaveClass(
      'track-fs-viewer-mono',
    );
    expect(
      screen.getByText('Worker returned non-zero exit status'),
    ).toHaveClass('track-fs-viewer-verdict-reason');
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

    mockTrackFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_payload/', kind: 'dir' }],
      'cards/card_payload': [{ name: '.payload.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
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

    mockTrackFileLists({
      '': [
        { name: 'report.md', kind: 'file' },
        { name: 'runs/', kind: 'dir', size: 1 },
      ],
      runs: [{ name: 'index.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /runs\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"not":"an array"}',
    );
    expect(
      screen.queryByRole('heading', { name: /Runs in this track/ }),
    ).not.toBeInTheDocument();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('falls back to raw JSON when cards/index.json is malformed', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    mockTrackFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'index.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"not":"an array"}',
    );
    expect(
      screen.queryByRole('heading', { name: /Cards in this track/ }),
    ).not.toBeInTheDocument();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('falls back to raw JSON when cards/index.json is invalid JSON', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    mockTrackFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'index.json', kind: 'file' }],
    });
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(await screen.findByRole('treeitem', { name: /index\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      'not valid json {{',
    );
    expect(
      screen.queryByRole('heading', { name: /Cards in this track/ }),
    ).not.toBeInTheDocument();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('falls back to raw JSON when cards/<id>/runtime.json is malformed', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    mockTrackFileLists({
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
    mockTrackFileContents({
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
      <TrackReportPage
        track={makeTrack()}
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

  it('resets the selected file to report.md when the track id changes', async () => {
    mockUseTrackFileContent.mockImplementation((trackId, requestedPath) => {
      if (requestedPath === 'report.md') {
        return contentResult({
          data: {
            content_type: 'text/markdown',
            content: trackId === 'track_B' ? '# New report' : '# Old report',
          },
        });
      }
      if (requestedPath === 'track.json') {
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
      <TrackReportPage
        track={makeTrack({ id: 'track_A' })}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /track\.json/ }));
    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      '{"ok":true}',
    );

    rerender(
      <TrackReportPage
        track={makeTrack({ id: 'track_B' })}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    await waitFor(() => {
      expect(
        screen.getByRole('treeitem', { name: /report\.md/ }),
      ).toHaveAttribute('aria-selected', 'true');
      expect(
        screen.getByRole('treeitem', { name: /track\.json/ }),
      ).toHaveAttribute('aria-selected', 'false');
      expect(
        screen.getByRole('heading', { level: 1, name: 'New report' }),
      ).toBeInTheDocument();
    });
  });

  it('does not query the previous file path under a new track id when switching tracks', () => {
    mockUseTrackFileContent.mockClear();
    mockUseTrackFileContent.mockReturnValue(
      contentResult({
        data: { content_type: 'text/markdown', content: '# A' },
      }),
    );

    const { rerender } = render(
      <TrackReportPage
        track={makeTrack({ id: 'track_A' })}
        cards={[reportSlot('A body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /track\.json/ }));

    mockUseTrackFileContent.mockClear();
    rerender(
      <TrackReportPage
        track={makeTrack({ id: 'track_B' })}
        cards={[reportSlot('B body')]}
      />,
    );

    const badCall = mockUseTrackFileContent.mock.calls.find(
      (args) => args[0] === 'track_B' && args[1] === 'track.json',
    );
    expect(badCall).toBeUndefined();
  });

  it('renders the inline loading state while file content is loading', () => {
    mockTrackFileContentForPath('report.md', { isLoading: true });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    expect(screen.getByRole('status')).toHaveTextContent('Loading…');
  });

  it('renders an inline error when a non-report file fails (no fallback)', () => {
    mockTrackFileContentForPath('track.json', {
      error: new CalmApiError(500, 'file_read_failed', 'File read failed'),
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /track\.json/ }));

    expect(screen.getByRole('alert')).toHaveTextContent('File read failed');
  });

  it('renders a distinct inline message for unsupported content types', () => {
    mockTrackFileContentForPath('report.md', {
      data: {
        content_type: 'image/png',
        content: '...',
      },
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

    expect(
      screen.getByText(/Preview unavailable for image\/png/i),
    ).toBeInTheDocument();
  });

  it('falls back to the report card body when report.md returns 404', () => {
    mockTrackFileContentForPath('report.md', {
      error: new CalmApiError(404, 'not_found', 'File not found'),
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('# Card fallback')]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Card fallback' }),
    ).toBeInTheDocument();
  });

  it('falls back to the report card body when report.md returns 500 (legacy)', () => {
    mockTrackFileContentForPath('report.md', {
      error: new CalmApiError(500, 'file_read_failed', 'File read failed'),
    });

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Card body **fallback**')]}
      />,
    );

    expect(screen.getByText('fallback').tagName).toBe('STRONG');
  });

  it('keeps projected task diagnostics and actions in the report.md error fallback', () => {
    mockUseTrackReportQuery.mockReturnValue({
      data: {
        schemaVersion: 1,
        docRev: 3,
        summary: '',
        body: 'Fallback with task',
        blocks: [{
          id: 'b_task', kind: 'task', rev: 2,
          payload: {
            key: 'ship', kind: 'codex', goal: 'Ship safely', ready: true,
            declared_by: 'spec', released_by_user: false,
          },
        }],
        taskDiagnostics: [{
          blockId: 'b_task', key: 'ship', schedulable: false, status: 'pending',
          gateResult: null, workerCardId: null,
          diagnostics: [{
            code: 'declare_and_wait', messageArgs: {}, relatedBlockIds: [],
            relatedTrackId: undefined,
            path: 'released_by_user', message: 'compat', action: 'release_task',
          }],
        }],
      },
      refetch: vi.fn(async () => ({ data: undefined })),
    } as unknown as ReturnType<typeof useTrackReportQuery>);
    mockTrackFileContentForPath('report.md', {
      error: new CalmApiError(500, 'file_read_failed', 'File read failed'),
    });

    render(<TrackReportPage track={makeTrack()} cards={[reportSlot('legacy')]} />);

    const task = screen.getByRole('region', { name: 'Task ship' });
    expect(within(task).getByRole('alert')).toHaveTextContent(
      'AI-proposed tasks in this track wait for you',
    );
    expect(within(task).getByRole('button', { name: 'Allow this task' }))
      .toBeEnabled();
  });

  it('keeps projected task diagnostics and actions in the report.md loading fallback', () => {
    mockUseTrackReportQuery.mockReturnValue({
      data: {
        schemaVersion: 1,
        docRev: 3,
        summary: '',
        body: 'Fallback with task',
        blocks: [{
          id: 'b_task', kind: 'task', rev: 2,
          payload: {
            key: 'ship', kind: 'codex', goal: 'Ship safely', ready: true,
            declared_by: 'spec', released_by_user: false,
          },
        }],
        taskDiagnostics: [{
          blockId: 'b_task', key: 'ship', schedulable: false, status: 'pending',
          gateResult: null, workerCardId: null,
          diagnostics: [{
            code: 'declare_and_wait', messageArgs: {}, relatedBlockIds: [],
            relatedTrackId: undefined,
            path: 'released_by_user', message: 'compat', action: 'release_task',
          }],
        }],
      },
      refetch: vi.fn(async () => ({ data: undefined })),
    } as unknown as ReturnType<typeof useTrackReportQuery>);
    mockTrackFileContentForPath('report.md', {
      isLoading: true,
      fetchStatus: 'fetching',
    });

    render(<TrackReportPage track={makeTrack()} cards={[reportSlot('legacy')]} />);

    const task = screen.getByRole('region', { name: 'Task ship' });
    expect(within(task).getByRole('alert')).toHaveTextContent(
      'AI-proposed tasks in this track wait for you',
    );
    expect(within(task).getByRole('button', { name: 'Allow this task' }))
      .toBeEnabled();
  });

  it('renders the conversation drawer closed by default with the report beside it', () => {
    const { container } = render(
      <TrackReportPage
        track={makeTrack()}
        cards={[plannerSlot(), reportSlot('Report with chat')]}
      />,
    );

    expect(screen.getByRole('button', { name: 'Open conversation' }))
      .toBeEnabled();
    expect(container.querySelector('.report-page')).not.toHaveClass(
      'report-page--conversation-open',
    );
    expect(screen.getByText('Report with chat')).toBeInTheDocument();
    expect(screen.getByLabelText('Ask the Planner Agent')).toBeInTheDocument();
  });

  it('opens the drawer without unmounting the report document', () => {
    const { container } = render(
      <TrackReportPage
        track={makeTrack()}
        cards={[plannerSlot(), reportSlot('Report with chat')]}
      />,
    );

    expect(screen.getByText('Report with chat')).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation' }),
    );

    expect(container.querySelector('.report-page')).toHaveClass(
      'report-page--conversation-open',
    );
    expect(screen.getByLabelText('Conversation')).toBeInTheDocument();
    expect(screen.getByText('Report with chat')).toBeInTheDocument();
  });

  it('does not offer a meaningless drawer entry without a planner card', () => {
    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report without planner')]}
      />,
    );

    expect(screen.queryByRole('button', { name: 'Open conversation' }))
      .not.toBeInTheDocument();
    expect(screen.getByText('This track has no Planner Agent.'))
      .toBeInTheDocument();
  });

  it('shows the unavailable empty drawer without a composer when persisted open', () => {
    window.localStorage.setItem(
      REPORT_CONVERSATION_COLLAPSED_STORAGE_KEY,
      'false',
    );

    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('Report without planner')]}
      />,
    );

    expect(screen.getByLabelText('Conversation drawer')).toHaveClass(
      'report-conversation-drawer--open',
    );
    expect(screen.getByText('Planner Agent is unavailable for this track.'))
      .toBeInTheDocument();
    expect(screen.queryByLabelText('Ask the Planner Agent'))
      .not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Open conversation' }))
      .not.toBeInTheDocument();
  });

  it('keeps the draft alive when the drawer closes and reopens', () => {
    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[plannerSlot(), reportSlot('Report with chat')]}
      />,
    );

    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation' }),
    );
    const draft = screen.getByLabelText('Ask the Planner Agent');
    fireEvent.change(draft, { target: { value: 'Persistent draft' } });

    fireEvent.click(
      screen.getByRole('button', { name: 'Close conversation' }),
    );
    expect(screen.getByLabelText('Ask the Planner Agent')).toBe(draft);
    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation' }),
    );
    expect(screen.getByLabelText('Ask the Planner Agent')).toHaveValue(
      'Persistent draft',
    );
  });

  it('persists the drawer state across remounts', () => {
    const props = {
      track: makeTrack(),
      cards: [plannerSlot(), reportSlot('Report with chat')],
    };
    const first = render(<TrackReportPage {...props} />);

    fireEvent.click(
      screen.getByRole('button', { name: 'Open conversation' }),
    );
    expect(
      window.localStorage.getItem(REPORT_CONVERSATION_COLLAPSED_STORAGE_KEY),
    ).toBe('false');

    first.unmount();
    render(<TrackReportPage {...props} />);
    expect(
      screen.getByRole('button', { name: 'Close conversation' }),
    ).toBeEnabled();
  });

  it('renders backlinks grouped by source track with source block anchors', () => {
    mockUseTrackBacklinksQuery.mockReturnValue({
      data: {
        backlinks: [{
          src_track_id: 'track_a',
          src_track_title: 'Alpha',
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
          src_track_id: 'track_a',
          src_track_title: 'Alpha',
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
          src_track_id: 'track_b',
          src_track_title: 'Beta',
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
    } as unknown as ReturnType<typeof useTrackBacklinksQuery>);

    render(
      <TrackReportPage
        track={makeTrack()}
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
      name: 'Alpha …context before First mention context after…',
    });
    expect(quotedLink).toHaveAttribute('href', '/track/track_a#b_a1');
    expect(within(quotedLink).getByText('First mention').tagName).toBe('B');
    const emptyLabelLink = within(panel).getByRole('link', {
      name: 'Alpha Empty label context remains readable',
    });
    expect(emptyLabelLink.querySelector('b')).toBeNull();
    expect(within(panel).getByRole('link', { name: 'Beta Another source' })).toHaveAttribute(
      'href',
      '/track/track_b#b_b1',
    );
  });

  it('labels self-references distinctly', () => {
    mockUseTrackBacklinksQuery.mockReturnValue({
      data: {
        backlinks: [{
          src_track_id: 'track_1',
          src_track_title: 'Planner track',
          src_block_id: 'b_source',
          dst_block_id: 'b_target',
          label: 'Self citation',
          updated_at: 1,
        }],
        truncated: false,
        skipped_sources: 0,
      },
      error: null,
    } as unknown as ReturnType<typeof useTrackBacklinksQuery>);

    render(<TrackReportPage track={makeTrack()} cards={[reportSlot('Report body')]} />);
    expect(screen.getByText('This track (self-reference)')).toBeInTheDocument();
  });

  it('does not promise a block target for a flat v1 report', () => {
    mockUseTrackBacklinksQuery.mockReturnValue({
      data: {
        backlinks: [{
          src_track_id: 'track_a',
          src_track_title: 'Alpha',
          src_block_id: 'b_a1',
          dst_block_id: 'b_here',
          label: 'Legacy target',
          updated_at: 1,
        }],
        truncated: false,
        skipped_sources: 0,
      },
      error: null,
    } as unknown as ReturnType<typeof useTrackBacklinksQuery>);

    render(
      <TrackReportPage track={makeTrack()} cards={[reportSlot('Report body')]} />,
    );
    expect(screen.getByRole('link', { name: 'Alpha Legacy target' }))
      .toBeVisible();
    expect(screen.queryByText(/cites block b_here/)).toBeNull();
  });

  it('surfaces backlink loading errors inline', () => {
    mockUseTrackBacklinksQuery.mockReturnValue({
      data: undefined,
      error: new Error('server unavailable'),
    } as unknown as ReturnType<typeof useTrackBacklinksQuery>);

    render(<TrackReportPage track={makeTrack()} cards={[reportSlot('Report body')]} />);
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Could not load backlinks: server unavailable',
    );
  });

  it('renders a neige link in a flat-body report as an in-app link', () => {
    render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('[Target](neige://wave/track_2#b_cafe)')]}
      />,
    );
    expect(screen.getByRole('link', { name: 'Target' })).toHaveAttribute(
      'href',
      '/track/track_2#b_cafe',
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
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot('fallback', { blocks: [block] })]}
      />,
    );
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    rerender(
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage track={makeTrack()} cards={[reportSlot('Loading report')]} />,
    );
    expect(scrollIntoView).not.toHaveBeenCalled();

    rerender(
      <TrackReportPage
        track={makeTrack()}
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
      <TrackReportPage track={makeTrack()} cards={[reportSlot('Report body')]} />,
    );
    const backlinks = screen.getByRole('region', { name: 'Backlinks' });
    expect(within(backlinks).getByText('No backlinks yet.')).toBeInTheDocument();
  });

  it('renders normally when the hash names a missing block', () => {
    window.history.replaceState(null, '', '#b_missing');
    expect(() =>
      render(
        <TrackReportPage
          track={makeTrack()}
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

  it("never renders a v1 body's maintenance contract (#1185)", () => {
    // The flat-body path renders the whole report source in one pass through
    // `MemoizedMarkdownBody`. A document that carries its own maintenance
    // contract as a leading HTML comment must render none of it — react-markdown
    // prints raw HTML as text unless `skipHtml` is set, and the comment spans
    // blank lines (CommonMark HTML block type 2), so the whole contract is one
    // raw node.
    // The kernel's own bytes, read off `crates/calm-types/src/track_report_*.md`
    // — a transcription would only prove this path hides *a* comment.
    const [contract, ...sections] = splitInitialBody();
    expect(contract.startsWith('<!-- 报告维护契约')).toBe(true);
    expect(contract.endsWith('-->\n\n')).toBe(true);
    expect(contract).toContain('散文正文');

    const { container } = render(
      <TrackReportPage
        track={makeTrack()}
        cards={[reportSlot(`${contract}${sections[0]}本轮结论。\n`)]}
      />,
    );

    expect(container.textContent).not.toContain('报告维护契约');
    expect(container.innerHTML).not.toContain('报告维护契约');
    expect(container.textContent).not.toContain('散文正文');
    expect(container.innerHTML).not.toContain('散文正文');
    expect(
      screen.getByRole('heading', { level: 1, name: '概要' }),
    ).toBeInTheDocument();
    expect(screen.getByText('本轮结论。')).toBeInTheDocument();
  });

  it('never renders the whole birth skeleton either (#1185)', () => {
    // The real v1 shape: `body` is exactly what a freshly minted track holds.
    const { container } = render(
      <TrackReportPage track={makeTrack()} cards={[reportSlot(initialBody())]} />,
    );

    expect(container.textContent).not.toContain('报告维护契约');
    expect(container.innerHTML).not.toContain('报告维护契约');
    expect(container.textContent).not.toContain('散文正文');
    expect(container.innerHTML).not.toContain('散文正文');
    // The first h1 is the page's own track title, not report content.
    expect(
      screen
        .getAllByRole('heading', { level: 1 })
        .map((h) => h.textContent)
        .slice(1),
    ).toEqual(['概要', '待你定', '已完成', '决策']);
  });
});
