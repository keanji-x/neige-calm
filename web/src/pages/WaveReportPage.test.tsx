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
  useWaveFileContent,
  useWaveFileList,
} from '../api/queries';
import { CalmApiError, type WaveFsContent, type WaveFsEntry } from '../api/calm';
import type { Wave, WaveCardSlot } from '../types';
import type { WaveReportCardData } from '../cards/builtins/wave-report';

vi.mock('../api/queries', () => ({
  useOverlaysByKindQuery: vi.fn(),
  useWaveFileList: vi.fn(),
  useWaveFileContent: vi.fn(),
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

const mockUseWaveFileList = vi.mocked(useWaveFileList);
const mockUseWaveFileContent = vi.mocked(useWaveFileContent);
const mockUseOverlaysByKindQuery = vi.mocked(useOverlaysByKindQuery);

const REPORT_RAIL_COLLAPSED_STORAGE_KEY = 'calm:report-rail:collapsed';

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
  window.localStorage.clear();
});

describe('WaveReportPage', () => {
  beforeEach(() => {
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

  it('renders the wave title and markdown body for one report card', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('The **answer** is ready.')]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Spec wave' }),
    ).toBeInTheDocument();
    expect(screen.getByText('answer').tagName).toBe('STRONG');
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

  it('renders a real Event line panel instead of the PR-E placeholder', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Event rail body')]}
      />,
    );

    const eventLine = screen.getByRole('region', { name: 'Event line' });
    expect(within(eventLine).getByText('Event line')).toBeInTheDocument();
    expect(within(eventLine).getByText('Nothing yet.')).toBeInTheDocument();
    expect(
      screen.queryByText('Activity timeline appears here. (Wired in PR-E.)'),
    ).not.toBeInTheDocument();
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

  it('renders the cards/<id>/meta.json wave fs viewer', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    mockWaveFileLists({
      '': [
        { name: 'cards/', kind: 'dir', size: 1 },
        { name: 'report.md', kind: 'file' },
      ],
      cards: [{ name: 'card_meta/', kind: 'dir' }],
      'cards/card_meta': [{ name: 'meta.json', kind: 'file' }],
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
      'cards/card_meta/meta.json': {
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

    fireEvent.click(await screen.findByRole('treeitem', { name: /cards\// }));
    fireEvent.click(
      await screen.findByRole('treeitem', { name: /codex card_met/ }),
    );
    fireEvent.click(await screen.findByRole('treeitem', { name: /meta\.json/ }));

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

  it('renders the conversation tab and input line when a spec card exists', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[specSlot(), reportSlot('Report with chat')]}
      />,
    );

    expect(screen.getByRole('button', { name: 'Conversation' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Report' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(screen.getByLabelText('Ask the Spec Agent')).toBeInTheDocument();
  });

  it('switches the column to the conversation document from the tab', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[specSlot(), reportSlot('Report with chat')]}
      />,
    );

    expect(screen.getByText('Report with chat')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Conversation' }));

    expect(
      screen.getByLabelText('Conversation'),
    ).toBeInTheDocument();
    expect(screen.queryByText('Report with chat')).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Report' }));

    expect(screen.getByText('Report with chat')).toBeInTheDocument();
  });

  it('disables the conversation tab and hides the input when no spec card exists', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Report without spec')]}
      />,
    );

    expect(screen.getByRole('button', { name: 'Conversation' })).toBeDisabled();
    expect(
      screen.queryByLabelText('Ask the Spec Agent'),
    ).not.toBeInTheDocument();
  });

  it('stays on the report view when the spec card disappears and reappears', () => {
    const wave = makeWave();
    const { rerender } = render(
      <WaveReportPage
        wave={wave}
        cards={[specSlot(), reportSlot('Report with chat')]}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Conversation' }));
    expect(screen.getByLabelText('Conversation')).toBeInTheDocument();

    // Spec card disappears: the column falls back to the report document.
    rerender(
      <WaveReportPage wave={wave} cards={[reportSlot('Report with chat')]} />,
    );
    expect(screen.getByText('Report with chat')).toBeInTheDocument();
    expect(screen.queryByLabelText('Conversation')).not.toBeInTheDocument();

    // Reappearance must not snap back to the stale conversation view.
    rerender(
      <WaveReportPage
        wave={wave}
        cards={[specSlot(), reportSlot('Report with chat')]}
      />,
    );
    expect(screen.getByText('Report with chat')).toBeInTheDocument();
    expect(screen.queryByLabelText('Conversation')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Report' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
  });
});
