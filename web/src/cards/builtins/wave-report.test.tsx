// Unit tests for the wave-report card (issue #229 PR B).
//
// Two surfaces under test:
//
//   1. `fromKernel` adapter contract — schemaVersion gating, zod
//      validation, happy path.
//   2. `parseSections` H1-splitting + the rendered component's
//      collapse behaviour, attention-section styling, and lifecycle
//      badge in the header.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, within } from '@testing-library/react';
import { WaveReportEntry, parseSections } from './wave-report';
import { WaveContext } from '../../shared/components/WaveContext';
import type { KernelCard } from '../../api/wire';
import type { WaveReportCardData } from '../../types';

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
        <WaveContext.Provider
          value={{ id: 'wave_test', lifecycle, title: 'Test Wave' }}
        >
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
