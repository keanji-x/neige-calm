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

function mockWaveFileContent(data: WaveFsContent) {
  mockUseWaveFileContent.mockReturnValue({
    data,
    error: null,
    isLoading: false,
  } as unknown as ReturnType<typeof useWaveFileContent>);
}

function mockWaveFileContentForPath(
  path: string,
  value: Partial<ReturnType<typeof useWaveFileContent>>,
) {
  mockUseWaveFileContent.mockImplementation((_, requestedPath) => {
    if (requestedPath === path) {
      return {
        data: undefined,
        error: null,
        isLoading: false,
        ...value,
      } as unknown as ReturnType<typeof useWaveFileContent>;
    }
    return {
      data: undefined,
      error: null,
      isLoading: false,
    } as unknown as ReturnType<typeof useWaveFileContent>;
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
    mockUseWaveFileContent.mockReturnValue({
      data: undefined,
      error: null,
      isLoading: false,
    } as unknown as ReturnType<typeof useWaveFileContent>);
  });

  it('renders the empty state when there is no report card', () => {
    render(<WaveReportPage wave={makeWave()} cards={[]} />);

    expect(screen.getByRole('status')).toHaveTextContent(
      'Report not ready. The spec agent has not produced a report yet.',
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

  it('resets the selected file when the wave id changes', async () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Wave file',
    });

    const { rerender } = render(
      <WaveReportPage
        wave={makeWave({ id: 'wave_A' })}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const reportFile = screen.getByRole('treeitem', { name: /report\.md/ });
    fireEvent.click(reportFile);
    expect(reportFile).toHaveAttribute('aria-selected', 'true');
    expect(
      screen.getByRole('dialog', { name: /file viewer: report\.md/i }),
    ).toBeInTheDocument();

    rerender(
      <WaveReportPage
        wave={makeWave({ id: 'wave_B' })}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    await waitFor(() => {
      expect(
        screen.getByRole('treeitem', { name: /report\.md/ }),
      ).toHaveAttribute('aria-selected', 'false');
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('opens a drawer with markdown content when a file is selected', () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Hi',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));

    const dialog = screen.getByRole('dialog', {
      name: /file viewer: report\.md/i,
    });
    expect(dialog).toBeInTheDocument();
    expect(
      within(dialog).getByRole('heading', { level: 1, name: 'Hi' }),
    ).toBeInTheDocument();
  });

  it('opens the drawer with Enter and requests content for the selected path', () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Hi',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const reportFile = screen.getByRole('treeitem', { name: /report\.md/ });
    fireEvent.keyDown(reportFile, { key: 'Enter' });

    expect(
      screen.getByRole('dialog', { name: /file viewer: report\.md/i }),
    ).toBeInTheDocument();
    expect(mockUseWaveFileContent).toHaveBeenCalledWith('wave_1', 'report.md', {
      enabled: true,
    });
  });

  it('marks the drawer as non-modal for assistive tech', () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Hi',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));

    expect(
      screen.getByRole('dialog', { name: /file viewer: report\.md/i }),
    ).toHaveAttribute('aria-modal', 'false');
  });

  it('closes the drawer with Escape and clears the selected file', async () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Hi',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const reportFile = screen.getByRole('treeitem', { name: /report\.md/ });
    fireEvent.click(reportFile);
    expect(reportFile).toHaveAttribute('aria-selected', 'true');

    fireEvent.keyDown(window, { key: 'Escape' });

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
      expect(reportFile).toHaveAttribute('aria-selected', 'false');
    });
  });

  it('returns focus to the tree row after closing with Escape', async () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Hi',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const reportFile = screen.getByRole('treeitem', { name: /report\.md/ });
    fireEvent.click(reportFile);
    fireEvent.keyDown(window, { key: 'Escape' });

    await waitFor(() => {
      expect(document.activeElement).toBe(reportFile);
    });
  });

  it('closes the drawer with the close button', async () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Hi',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));
    fireEvent.click(
      screen.getByRole('button', { name: /close file viewer/i }),
    );

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('returns focus to the tree row after closing with the close button', async () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Hi',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const reportFile = screen.getByRole('treeitem', { name: /report\.md/ });
    fireEvent.click(reportFile);
    fireEvent.click(
      screen.getByRole('button', { name: /close file viewer/i }),
    );

    await waitFor(() => {
      expect(document.activeElement).toBe(reportFile);
    });
  });

  it('closes the drawer from the backdrop', async () => {
    mockWaveFileContent({
      content_type: 'text/markdown',
      content: '# Hi',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));
    const dialog = screen.getByRole('dialog', {
      name: /file viewer: report\.md/i,
    });
    const backdrop = dialog.parentElement;
    expect(backdrop).toHaveClass('wave-file-drawer-backdrop');

    fireEvent.mouseDown(backdrop!);

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('renders non-markdown text content with CodePane', async () => {
    mockWaveFileContent({
      content_type: 'text/plain',
      content: 'plain text file',
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /wave\.json/ }));

    expect(await screen.findByTestId('code-pane')).toHaveTextContent(
      'plain text file',
    );
  });

  it('renders the drawer loading state while file content is loading', () => {
    mockWaveFileContentForPath('report.md', { isLoading: true });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));

    expect(screen.getByText('Loading...')).toBeInTheDocument();
  });

  it('renders the drawer error state when file content fails to load', () => {
    mockWaveFileContentForPath('report.md', {
      error: new CalmApiError(500, 'file_read_failed', 'File read failed'),
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));

    expect(screen.getByRole('alert')).toHaveTextContent('File read failed');
  });

  it('renders a distinct message for unsupported content types', () => {
    mockWaveFileContentForPath('report.md', {
      data: {
        content_type: 'image/png',
        content: '...',
      },
    });

    render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    fireEvent.click(screen.getByRole('treeitem', { name: /report\.md/ }));

    expect(
      screen.getByText(/Preview unavailable for image\/png/i),
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
