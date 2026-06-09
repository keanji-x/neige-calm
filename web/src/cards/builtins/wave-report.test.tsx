// Unit tests for the wave-report card (issue #229 PR B + #247 PR4).
//
// Three surfaces under test:
//
//   1. `fromKernel` adapter contract — schemaVersion gating, zod
//      validation, happy path.
//   2. `parseSections` H1-splitting + the rendered component's
//      collapse behaviour, attention-section styling, and lifecycle
//      badge in the header.
//   3. Inline edit mode (#247 PR4) — pencil button toggles to editor,
//      Save posts to the REST endpoint and adopts the projected
//      payload, Cancel discards, error responses keep the user in
//      edit mode with a visible error string.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import type { ReactNode } from 'react';

// Hoisted mock for the api client. Only the wave-report endpoint is
// exercised here; we still need to passthrough `CalmApiError` because
// the component imports the class to type-narrow error responses.
vi.mock('../../api/calm', async () => {
  const actual =
    await vi.importActual<typeof import('../../api/calm')>('../../api/calm');
  return {
    ...actual,
    updateWaveReport: vi.fn(),
  };
});

vi.mock('./wave-report-sidebar', () => ({
  WaveReportSidebar: ({ fallback }: { fallback?: ReactNode }) => (
    <div data-testid="mock-wave-report-sidebar">{fallback}</div>
  ),
}));

import { WaveReportEntry, parseSections, type WaveReportCardData } from './wave-report';
import { WaveContext } from '../../shared/components/WaveContext';
import * as api from '../../api/calm';
import type { KernelCard } from '../../api/wire';

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'report_1',
    wave_id: 'wave_1',
    kind: 'wave-report',
    sort: -1,
    payload: {
      schemaVersion: 1,
      summary: 'one-line summary',
      body: '# Goal\n\nrefactor the dispatcher\n\n# Progress\n\n- did X\n- doing Y\n\n# Timeline\n\n2025-05-23: started\n',
    },
    deletable: false,
    created_at: 1000,
    updated_at: 2000,
    ...over,
  };
}

describe('parseSections', () => {
  it('splits at H1 headings, preserves body order', () => {
    const out = parseSections('# Goal\n\na\n\n# Progress\n\nb\n');
    expect(out.map((s) => s.title)).toEqual(['Goal', 'Progress']);
    expect(out[0].body.trim()).toBe('a');
    expect(out[1].body.trim()).toBe('b');
    expect(out[0].slug).toBe('goal');
    expect(out[1].slug).toBe('progress');
  });

  it('captures content before the first H1 as a _preamble section', () => {
    const out = parseSections('intro line\n\n# Goal\n\nstuff\n');
    expect(out[0].slug).toBe('_preamble');
    expect(out[0].body.trim()).toBe('intro line');
    expect(out[1].slug).toBe('goal');
  });

  it('returns [] for an empty body', () => {
    expect(parseSections('')).toEqual([]);
  });

  it('slugifies titles for stable keys', () => {
    const out = parseSections('# Needs attention\n\nx\n');
    expect(out[0].slug).toBe('needs-attention');
  });
});

describe('WaveReportEntry.fromKernel', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });
  afterEach(() => {
    warnSpy.mockRestore();
  });

  it('claims kind=wave-report', () => {
    const out = WaveReportEntry.fromKernel!(makeKernelCard());
    expect(out).toMatchObject({
      type: 'wave-report',
      id: 'report_1',
      summary: 'one-line summary',
    });
    expect(out!.body).toContain('# Goal');
  });

  it('returns null for other kinds', () => {
    const out = WaveReportEntry.fromKernel!(
      makeKernelCard({ kind: 'codex', payload: {} }),
    );
    expect(out).toBeNull();
  });

  it('returns null for invalid payload (missing body)', () => {
    const out = WaveReportEntry.fromKernel!(
      makeKernelCard({ payload: { schemaVersion: 1, summary: 'hi' } }),
    );
    expect(out).toBeNull();
    expect(warnSpy).toHaveBeenCalled();
  });

  it('emits unsupportedVersion when schemaVersion > 1', () => {
    const out = WaveReportEntry.fromKernel!(
      makeKernelCard({
        payload: { schemaVersion: 99, summary: '', body: 'x' },
      }),
    );
    expect(out).toMatchObject({
      type: 'wave-report',
      unsupportedVersion: 99,
    });
    expect(warnSpy).toHaveBeenCalled();
  });

  it('accepts payload with missing schemaVersion (treated as v1)', () => {
    const out = WaveReportEntry.fromKernel!(
      makeKernelCard({ payload: { summary: '', body: '# G\n' } }),
    );
    expect(out).toMatchObject({ type: 'wave-report' });
  });
});

describe('WaveReportCard rendering', () => {
  // localStorage is per-test (jsdom resets between tests when we
  // don't share state; we clear explicitly to be safe).
  beforeEach(() => {
    try {
      window.localStorage.clear();
    } catch {
      // ignore
    }
  });

  function renderWithContext(
    card: WaveReportCardData,
    {
      lifecycle = 'planning' as const,
      withContext = true,
      onClose,
    }: {
      lifecycle?: 'draft' | 'planning' | 'working' | 'blocked' | 'reviewing' | 'done';
      withContext?: boolean;
      onClose?: () => void;
    } = {},
  ) {
    const Component = WaveReportEntry.Component;
    if (withContext) {
      return render(
        <WaveContext.Provider value={{ id: 'wave_test', lifecycle }}>
          <Component card={card} onClose={onClose} />
        </WaveContext.Provider>,
      );
    }
    return render(<Component card={card} onClose={onClose} />);
  }

  it('renders sections from the markdown body', () => {
    renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: 'summary line',
      body: '# Goal\n\nrefactor\n\n# Progress\n\n- a\n- b\n',
    });
    expect(screen.getByText('Goal')).toBeTruthy();
    expect(screen.getByText('Progress')).toBeTruthy();
    expect(screen.getByText('refactor')).toBeTruthy();
    expect(screen.getByText('a')).toBeTruthy();
    expect(screen.getByText('b')).toBeTruthy();
  });

  it('renders the wave summary line above sections when non-empty', () => {
    renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: 'this is the preview',
      body: '# Goal\n\nx\n',
    });
    expect(screen.getByText('this is the preview')).toBeTruthy();
  });

  it('omits the summary line when empty', () => {
    renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: '   ',
      body: '# Goal\n\nx\n',
    });
    expect(screen.queryByLabelText('Wave report summary')).toBeNull();
  });

  it('Timeline section is collapsed by default; Goal is open', () => {
    renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '# Goal\n\nfirstline\n\n# Timeline\n\nlastline\n',
    });
    // Goal: body present.
    expect(screen.getByText('firstline')).toBeTruthy();
    // Timeline: body absent (collapsed). The heading still shows.
    expect(screen.getByText('Timeline')).toBeTruthy();
    expect(screen.queryByText('lastline')).toBeNull();
  });

  it('toggle on a section expands it and persists to localStorage', () => {
    renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '# Goal\n\nopen\n\n# Timeline\n\nhidden\n',
    });
    // Click the Timeline section's toggle button.
    const timelineHeading = screen.getByText('Timeline');
    const toggle = timelineHeading.closest('button');
    expect(toggle).toBeTruthy();
    fireEvent.click(toggle!);
    expect(screen.getByText('hidden')).toBeTruthy();
    // Persistence: storage key flipped to 0 (uncollapsed).
    expect(window.localStorage.getItem('wave-report:wave_test:section:timeline:collapsed')).toBe('0');
  });

  it('applies the "attention" CSS class to Needs attention sections', () => {
    const { container } = renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '# Needs attention\n\nblocked on review\n\n# Progress\n\nx\n',
    });
    const attention = container.querySelector('.wave-report-section.attention');
    expect(attention).toBeTruthy();
    expect(within(attention as HTMLElement).getByText('Needs attention')).toBeTruthy();
    // Progress is not flagged.
    const progress = Array.from(
      container.querySelectorAll('.wave-report-section'),
    ).find((el) => within(el as HTMLElement).queryByText('Progress'));
    expect(progress).toBeTruthy();
    expect((progress as HTMLElement).classList.contains('attention')).toBe(false);
  });

  it('renders the lifecycle badge from WaveContext', () => {
    renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '# Goal\n\nx\n',
    }, { lifecycle: 'reviewing' });
    // WaveLifecycleBadge renders the label "In review" for reviewing.
    expect(screen.getByText('In review')).toBeTruthy();
  });

  it('omits the lifecycle badge when no WaveContext is provided', () => {
    renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '# Goal\n\nx\n',
    }, { withContext: false });
    // No lifecycle badge means no `In review` / `Planning` / etc.
    expect(screen.queryByText('Planning')).toBeNull();
    expect(screen.queryByText('In review')).toBeNull();
  });

  it('renders the unsupported-version fallback', () => {
    renderWithContext({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '',
      unsupportedVersion: 99,
    });
    expect(screen.getByText(/Unsupported card payload version/i)).toBeTruthy();
  });

  it('forwards onClose to the CardHead close button', () => {
    const onClose = vi.fn();
    renderWithContext(
      {
        type: 'wave-report',
        id: 'r1',
        summary: '',
        body: '# Goal\n\nx\n',
      },
      { onClose },
    );
    const closeBtn = screen.getByLabelText('Remove panel');
    fireEvent.click(closeBtn);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

// ---- PR4 of #247: inline edit mode ----------------------------------

describe('WaveReportCard edit mode (#247 PR4)', () => {
  const updateMock = api.updateWaveReport as ReturnType<typeof vi.fn>;

  beforeEach(() => {
    updateMock.mockReset();
    try {
      window.localStorage.clear();
    } catch {
      // ignore
    }
  });

  function renderEditable(card: WaveReportCardData) {
    const Component = WaveReportEntry.Component;
    return render(
      <WaveContext.Provider value={{ id: 'wave_edit_test', lifecycle: 'planning' }}>
        <Component card={card} />
      </WaveContext.Provider>,
    );
  }

  it('renders read-only by default with an edit affordance', () => {
    renderEditable({
      type: 'wave-report',
      id: 'r1',
      summary: 'initial summary',
      body: '# Goal\n\nbody\n',
    });
    expect(screen.getByText('Goal')).toBeTruthy();
    // Edit affordance present, body textarea is not.
    expect(screen.getByLabelText('Edit report')).toBeTruthy();
    expect(screen.queryByLabelText('Wave report body')).toBeNull();
  });

  it('omits the edit button when no WaveContext is provided', () => {
    const Component = WaveReportEntry.Component;
    render(
      <Component
        card={{
          type: 'wave-report',
          id: 'r1',
          summary: '',
          body: '# Goal\n\nx\n',
        }}
      />,
    );
    expect(screen.queryByLabelText('Edit report')).toBeNull();
  });

  it('clicking the pencil enters edit mode with the body preloaded; no summary input is rendered', () => {
    renderEditable({
      type: 'wave-report',
      id: 'r1',
      summary: 'initial summary',
      body: '# Goal\n\nbody text\n',
    });
    fireEvent.click(screen.getByLabelText('Edit report'));
    const bodyTextarea = screen.getByLabelText('Wave report body') as HTMLTextAreaElement;
    expect(bodyTextarea.value).toBe('# Goal\n\nbody text\n');
    // Summary is AI-maintained — edit mode must NOT surface any
    // control (input/textbox/label) for it. The read-mode summary
    // div is also not rendered while editing (read/edit views are a
    // ternary, not stacked), so the aria-label lookup is null here.
    expect(screen.queryByLabelText('Wave report summary')).toBeNull();
    expect(screen.queryByRole('textbox', { name: 'Wave report summary' })).toBeNull();
    // The pencil button is hidden while editing so the user can't
    // accidentally re-enter edit mode mid-flight.
    expect(screen.queryByLabelText('Edit report')).toBeNull();
  });

  it('Cancel exits edit mode and discards typed edits', () => {
    renderEditable({
      type: 'wave-report',
      id: 'r1',
      summary: 'orig',
      body: '# Goal\n\norig\n',
    });
    fireEvent.click(screen.getByLabelText('Edit report'));
    const bodyTextarea = screen.getByLabelText('Wave report body') as HTMLTextAreaElement;
    fireEvent.change(bodyTextarea, { target: { value: '# Discarded\n\ngone\n' } });
    fireEvent.click(screen.getByText('Cancel'));
    // Back in read-only mode showing the original content.
    expect(screen.queryByLabelText('Wave report body')).toBeNull();
    expect(screen.getByText('Goal')).toBeTruthy();
    expect(screen.queryByText('Discarded')).toBeNull();
    expect(updateMock).not.toHaveBeenCalled();
  });

  it('Save posts {summary unchanged, body edited} and adopts the projected payload on success', async () => {
    updateMock.mockResolvedValueOnce({
      schemaVersion: 1,
      summary: 'normalised summary',
      body: '# Goal\n\nnormalised body\n',
    });
    renderEditable({
      type: 'wave-report',
      id: 'r1',
      summary: 'orig summary',
      body: '# Goal\n\norig body\n',
    });
    fireEvent.click(screen.getByLabelText('Edit report'));
    const bodyTextarea = screen.getByLabelText('Wave report body') as HTMLTextAreaElement;
    fireEvent.change(bodyTextarea, { target: { value: '# Goal\n\nedited body\n' } });
    await act(async () => {
      fireEvent.click(screen.getByText('Save'));
    });
    expect(updateMock).toHaveBeenCalledTimes(1);
    // `summary` is sent through unchanged — the AI repopulates it on
    // the next `report.write`, the user never sees an input for it.
    expect(updateMock).toHaveBeenCalledWith('wave_edit_test', {
      summary: 'orig summary',
      body: '# Goal\n\nedited body\n',
    });
    // Back in read-only mode, displaying the *projected* content
    // (not the locally-typed copy — the server may have normalised).
    await waitFor(() => {
      expect(screen.queryByLabelText('Wave report body')).toBeNull();
    });
    expect(screen.getByText('normalised summary')).toBeTruthy();
    expect(screen.getByText(/normalised body/)).toBeTruthy();
  });

  it('typing in body does not mutate the submitted summary (invariant)', async () => {
    updateMock.mockResolvedValueOnce({
      schemaVersion: 1,
      summary: 'preserved',
      body: '# Goal\n\nv2\n',
    });
    renderEditable({
      type: 'wave-report',
      id: 'r1',
      summary: 'preserved',
      body: '# Goal\n\nv1\n',
    });
    fireEvent.click(screen.getByLabelText('Edit report'));
    const bodyTextarea = screen.getByLabelText('Wave report body') as HTMLTextAreaElement;
    // Multiple keystrokes — any one of them could conceivably leak
    // into the submitted summary if the wire-up regresses.
    fireEvent.change(bodyTextarea, { target: { value: '# Goal\n\nv1.5\n' } });
    fireEvent.change(bodyTextarea, { target: { value: '# Goal\n\nv1.6 with summary: not the summary\n' } });
    fireEvent.change(bodyTextarea, { target: { value: '# Goal\n\nv2\n' } });
    await act(async () => {
      fireEvent.click(screen.getByText('Save'));
    });
    expect(updateMock).toHaveBeenCalledTimes(1);
    const [, payload] = updateMock.mock.calls[0];
    expect(payload).toEqual({
      summary: 'preserved',
      body: '# Goal\n\nv2\n',
    });
  });

  it('403 from the server keeps the user in edit mode with their typed content + a visible error', async () => {
    const { CalmApiError } = await import('../../api/calm');
    updateMock.mockRejectedValueOnce(
      new CalmApiError(403, 'forbidden', 'spec-only edit'),
    );
    renderEditable({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '# Goal\n\norig\n',
    });
    fireEvent.click(screen.getByLabelText('Edit report'));
    const bodyTextarea = screen.getByLabelText('Wave report body') as HTMLTextAreaElement;
    fireEvent.change(bodyTextarea, { target: { value: '# Goal\n\ntyped edit\n' } });
    await act(async () => {
      fireEvent.click(screen.getByText('Save'));
    });
    // Still in edit mode; typed content preserved; error visible.
    await waitFor(() => {
      expect(screen.getByText('无权编辑此报告')).toBeTruthy();
    });
    expect((screen.getByLabelText('Wave report body') as HTMLTextAreaElement).value).toBe(
      '# Goal\n\ntyped edit\n',
    );
  });

  it('5xx from the server shows a generic retry message', async () => {
    const { CalmApiError } = await import('../../api/calm');
    updateMock.mockRejectedValueOnce(
      new CalmApiError(500, 'internal', 'boom'),
    );
    renderEditable({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '# Goal\n\nx\n',
    });
    fireEvent.click(screen.getByLabelText('Edit report'));
    await act(async () => {
      fireEvent.click(screen.getByText('Save'));
    });
    await waitFor(() => {
      expect(screen.getByText('保存失败，请重试')).toBeTruthy();
    });
  });

  it('disables Save while the request is in flight', async () => {
    let resolve!: (v: { schemaVersion: number; summary: string; body: string }) => void;
    updateMock.mockReturnValueOnce(
      new Promise((r) => {
        resolve = r;
      }),
    );
    renderEditable({
      type: 'wave-report',
      id: 'r1',
      summary: '',
      body: '# Goal\n\nx\n',
    });
    fireEvent.click(screen.getByLabelText('Edit report'));
    fireEvent.click(screen.getByText('Save'));
    // Button label flips to the saving spinner copy and is disabled.
    const saving = await screen.findByText('保存中…');
    expect(saving).toBeTruthy();
    expect((saving.closest('button') as HTMLButtonElement).disabled).toBe(true);
    // Settle the promise so the act warnings don't fire after teardown.
    await act(async () => {
      resolve({ schemaVersion: 1, summary: '', body: '# Goal\n\nx\n' });
    });
  });
});
