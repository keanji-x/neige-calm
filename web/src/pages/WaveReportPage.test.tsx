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
import type { WaveFsEntry } from '../api/calm';
import type { Wave, WaveCardSlot } from '../types';
import type { WaveReportCardData } from '../cards/builtins/wave-report';

vi.mock('../api/queries', () => ({
  useWaveFileList: vi.fn(),
  useWaveFileContent: vi.fn(),
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

  it('renders report findings directives', () => {
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[
          reportSlot(
            ':::findings\n::row[Directive **finding**.]{stat="2" unit="signals"}\n:::\n',
          ),
        ]}
      />,
    );

    expect(container.querySelector('.findings')).toBeInTheDocument();
    expect(container.querySelector('.find-row')).toHaveTextContent(
      'Directive finding.',
    );
  });

  it('renders GFM footnotes as report citation refs', () => {
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('Claim with source.[^1]\n\n[^1]: Source note.')]}
      />,
    );

    const ref = container.querySelector('sup .report-ref');
    expect(ref).toBeInTheDocument();
    expect(ref).toHaveAttribute('href', '#fn-1');
    expect(ref).toHaveTextContent('[1]');
  });

  it('renders raw script tags as text, not executable elements', () => {
    const { container } = render(
      <WaveReportPage
        wave={makeWave()}
        cards={[reportSlot('<script>alert(1)</script>')]}
      />,
    );

    expect(container.querySelector('script')).not.toBeInTheDocument();
    expect(screen.getByText('<script>alert(1)</script>')).toBeInTheDocument();
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
    const { rerender } = render(
      <WaveReportPage
        wave={makeWave({ id: 'wave_A' })}
        cards={[reportSlot('Files rail body')]}
      />,
    );

    const reportFile = screen.getByRole('treeitem', { name: /report\.md/ });
    fireEvent.click(reportFile);
    expect(reportFile).toHaveAttribute('aria-selected', 'true');

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
    });
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
