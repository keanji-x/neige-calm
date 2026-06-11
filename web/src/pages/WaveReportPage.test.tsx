import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { WaveReportPage } from './WaveReportPage';
import { useWaveFileContent, useWaveFileList } from '../api/queries';
import { CalmApiError, type WaveFsContent, type WaveFsEntry } from '../api/calm';
import type { Wave, WaveCardSlot } from '../types';
import type { WaveReportCardData } from '../cards/builtins/wave-report';

vi.mock('../api/queries', () => ({
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

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'wave_1',
    coveId: 'cove_1',
    title: 'Research wave',
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

afterEach(() => {
  vi.restoreAllMocks();
});

describe('WaveReportPage', () => {
  beforeEach(() => {
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

  it('renders the wave title and markdown body for one report card', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('The **answer** is ready.')]}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Research wave' }),
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

  it('renders the inline error state when file content fails to load', () => {
    mockWaveFileContentForPath('report.md', {
      error: new CalmApiError(500, 'file_read_failed', 'File read failed'),
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Fallback report body')]}
      />,
    );

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

  it('renders the SpecCurrentRun collapsed pill when a spec card exists', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[specSlot(), reportSlot('Report with chat')]}
      />,
    );

    expect(
      screen.getByRole('button', { name: 'Ask the Research Agent' }),
    ).toBeInTheDocument();
  });

  it('renders the unavailable chat placeholder when no spec card exists', () => {
    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Report without spec')]}
      />,
    );

    expect(screen.getByText('Spec agent unavailable')).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Ask the Research Agent' }),
    ).not.toBeInTheDocument();
  });
});
