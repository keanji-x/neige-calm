import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { RunsIndexViewer } from './runs-index-viewer';

const Component = RunsIndexViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
});

describe('RunsIndexViewer', () => {
  it('renders run rows with status, verdict, and timestamps', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    render(
      <Component
        path="runs/index.json"
        raw="[]"
        data={[
          {
            idempotency_key: 'run_1',
            status: 'completed',
            kind: 'codex',
            verdict: {
              status: 'accepted',
              at: new Date('2026-06-10T11:10:00Z').getTime(),
            },
            requested_at: new Date('2026-06-10T10:00:00Z').getTime(),
            finished_at: new Date('2026-06-10T11:00:00Z').getTime(),
            worker_card_id: 'card_worker_1',
          },
        ]}
      />,
    );

    expect(
      screen.getByRole('heading', { name: 'Runs in this wave (1)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('codex')).toHaveClass('wave-fs-viewer-primary');
    expect(screen.getByText('run_1')).toHaveClass('wave-fs-viewer-mono');
    expect(screen.getByText('completed')).toHaveAttribute(
      'data-tone',
      'success',
    );
    expect(screen.getByText('accepted')).toHaveAttribute(
      'data-tone',
      'success',
    );
    expect(screen.getByText('Requested 2h ago')).toBeInTheDocument();
    expect(screen.getByText('Finished 1h ago')).toBeInTheDocument();
  });

  it('renders an empty state and optional placeholders', () => {
    const { rerender } = render(
      <Component data={[]} path="runs/index.json" raw="[]" />,
    );

    expect(
      screen.getByRole('heading', { name: 'Runs in this wave (0)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('No runs yet.')).toHaveClass(
      'wave-fs-viewer-empty',
    );

    rerender(
      <Component
        path="runs/index.json"
        raw="[]"
        data={[
          {
            idempotency_key: 'run_min',
            status: 'unknown',
            kind: 'terminal',
            verdict: null,
            requested_at: null,
            finished_at: null,
            worker_card_id: null,
          },
        ]}
      />,
    );

    expect(screen.getByText('terminal')).toHaveClass('wave-fs-viewer-primary');
    expect(screen.getByText('Requested -')).toBeInTheDocument();
    expect(screen.getByText('Finished -')).toBeInTheDocument();
  });

  it('throws when required fields are missing', () => {
    expect(() => RunsIndexViewer.parse('{"id":"run_1"}')).toThrow();
    expect(() => RunsIndexViewer.parse('[{"status":"running"}]')).toThrow();
    expect(() =>
      RunsIndexViewer.parse(
        JSON.stringify([
          {
            idempotency_key: 'run_1',
            status: 'completed',
            kind: 'codex',
            verdict: {
              status: 'accepted',
            },
          },
        ]),
      ),
    ).toThrow();
  });
});
