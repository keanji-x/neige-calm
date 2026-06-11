import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { RunDetailViewer } from './run-detail-viewer';

const Component = RunDetailViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
});

describe('RunDetailViewer', () => {
  it('renders run details without expanding events or payload', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    render(
      <Component
        path="runs/run_1.json"
        data={{
          idempotencyKey: 'run_1',
          status: 'failed',
          kind: 'terminal',
          verdict: {
            status: 'rejected',
            at: new Date('2026-06-10T11:10:00Z').getTime(),
          },
          requestedAt: new Date('2026-06-10T10:00:00Z').getTime(),
          finishedAt: new Date('2026-06-10T11:00:00Z').getTime(),
          workerCardId: 'card_worker_1',
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'terminal' })).toHaveClass(
      'wave-fs-viewer-primary',
    );
    expect(screen.getByText('run_1')).toHaveClass('wave-fs-viewer-mono');
    expect(screen.getByText('failed')).toHaveAttribute('data-tone', 'danger');
    expect(screen.getByText('rejected')).toHaveAttribute('data-tone', 'danger');
    expect(screen.getByText('Requested 2h ago')).toBeInTheDocument();
    expect(screen.getByText('Finished 1h ago')).toBeInTheDocument();
    expect(screen.getByText('card_worker_1')).toHaveClass('wave-fs-viewer-mono');
    expect(screen.getByText('events / payload: see raw JSON')).toHaveClass(
      'wave-fs-viewer-note',
    );
  });

  it('renders placeholders with all optional fields missing', () => {
    const { container } = render(
      <Component
        path="runs/run_1.json"
        data={{
          idempotencyKey: 'run_min',
          status: 'requested',
          kind: 'codex',
        }}
      />,
    );

    expect(screen.getByText('Requested -')).toBeInTheDocument();
    expect(screen.getByText('Finished -')).toBeInTheDocument();
    expect(container).not.toHaveTextContent('worker');
    expect(screen.queryByText('accepted')).toBeNull();
  });

  it('throws when required fields are missing', () => {
    expect(() => RunDetailViewer.parse('[{"id":"run_1"}]')).toThrow(
      /must be an object/,
    );
    expect(() => RunDetailViewer.parse('{"status":"running"}')).toThrow(
      /idempotency_key string/,
    );
  });
});
