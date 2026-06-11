import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { RunDetailViewer } from './run-detail-viewer';

const Component = RunDetailViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
});

describe('RunDetailViewer', () => {
  it('renders run details with a closed full-payload disclosure', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );
    const raw = JSON.stringify({
      idempotency_key: 'run_1',
      status: 'failed',
      kind: 'terminal',
      verdict: {
        status: 'rejected',
        reason: 'Worker returned non-zero exit status',
        at: new Date('2026-06-10T11:10:00Z').getTime(),
      },
      events: { failed: { event_id: 12 } },
      worker_card_payload: { command: 'npm test' },
    });

    render(
      <Component
        path="runs/run_1.json"
        raw={raw}
        data={{
          idempotencyKey: 'run_1',
          status: 'failed',
          kind: 'terminal',
          verdict: {
            status: 'rejected',
            reason: 'Worker returned non-zero exit status',
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
    expect(
      screen.getByText('Worker returned non-zero exit status'),
    ).toHaveClass('wave-fs-viewer-verdict-reason');
    expect(screen.getByText('Requested 2h ago')).toBeInTheDocument();
    expect(screen.getByText('Finished 1h ago')).toBeInTheDocument();
    expect(screen.getByText('card_worker_1')).toHaveClass('wave-fs-viewer-mono');
    const summary = screen.getByText('Full payload (events, worker card)');
    const details = summary.closest('details');
    expect(details).not.toHaveAttribute('open');
    expect(details?.querySelector('code')).toHaveTextContent(raw);
    expect(
      screen.queryByText('events / payload: see raw JSON'),
    ).not.toBeInTheDocument();
  });

  it('parses verdict reason and requires verdict.at when verdict exists', () => {
    const verdictAt = new Date('2026-06-10T11:10:00Z').getTime();

    expect(
      RunDetailViewer.parse(
        JSON.stringify({
          idempotency_key: 'run_1',
          status: 'completed',
          kind: 'codex',
          verdict: {
            status: 'accepted',
            reason: 'Done',
            at: verdictAt,
          },
        }),
      ),
    ).toMatchObject({
      verdict: {
        status: 'accepted',
        reason: 'Done',
        at: verdictAt,
      },
    });
    expect(() =>
      RunDetailViewer.parse(
        JSON.stringify({
          idempotency_key: 'run_1',
          status: 'completed',
          kind: 'codex',
          verdict: {
            status: 'accepted',
          },
        }),
      ),
    ).toThrow(/at number/);
  });

  it('renders placeholders with all optional fields missing', () => {
    const { container } = render(
      <Component
        path="runs/run_1.json"
        raw="{}"
        data={{
          idempotencyKey: 'run_min',
          status: 'requested',
          kind: 'codex',
        }}
      />,
    );

    expect(screen.getByText('Requested -')).toBeInTheDocument();
    expect(screen.getByText('Finished -')).toBeInTheDocument();
    expect(container.querySelector('.wave-fs-viewer-field')).toBeNull();
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
