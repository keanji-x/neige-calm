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
      requested_at: new Date('2026-06-10T10:00:00Z').getTime(),
      finished_at: new Date('2026-06-10T11:00:00Z').getTime(),
      worker_card_id: 'card_worker_1',
      events: {
        requested: null,
        completed: null,
        failed: {
          created_at: new Date('2026-06-10T11:00:00Z').getTime(),
          event_id: 12,
          kind: 'worker.failed',
          payload: { exit_code: 1 },
        },
        verdict: null,
      },
      worker_card_payload: { command: 'npm test' },
    });

    render(
      <Component
        path="runs/run_1.json"
        raw={raw}
        data={{
          idempotency_key: 'run_1',
          status: 'failed',
          kind: 'terminal',
          verdict: {
            status: 'rejected',
            reason: 'Worker returned non-zero exit status',
            at: new Date('2026-06-10T11:10:00Z').getTime(),
          },
          requested_at: new Date('2026-06-10T10:00:00Z').getTime(),
          finished_at: new Date('2026-06-10T11:00:00Z').getTime(),
          worker_card_id: 'card_worker_1',
          events: {
            requested: null,
            completed: null,
            failed: {
              created_at: new Date('2026-06-10T11:00:00Z').getTime(),
              event_id: 12,
              kind: 'worker.failed',
              payload: { exit_code: 1 },
            },
            verdict: null,
          },
          worker_card_payload: { command: 'npm test' },
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
          requested_at: null,
          finished_at: null,
          worker_card_id: null,
          verdict: {
            status: 'accepted',
            reason: 'Done',
            at: verdictAt,
          },
          events: {
            requested: null,
            completed: null,
            failed: null,
            verdict: null,
          },
          worker_card_payload: null,
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
          requested_at: null,
          finished_at: null,
          worker_card_id: null,
          verdict: {
            status: 'accepted',
          },
          events: {
            requested: null,
            completed: null,
            failed: null,
            verdict: null,
          },
          worker_card_payload: null,
        }),
      ),
    ).toThrow();
  });

  it('renders placeholders with all nullable fields empty', () => {
    const { container } = render(
      <Component
        path="runs/run_1.json"
        raw="{}"
        data={{
          idempotency_key: 'run_min',
          status: 'requested',
          kind: 'codex',
          verdict: null,
          requested_at: null,
          finished_at: null,
          worker_card_id: null,
          events: {
            requested: null,
            completed: null,
            failed: null,
            verdict: null,
          },
          worker_card_payload: null,
        }}
      />,
    );

    expect(screen.getByText('Requested -')).toBeInTheDocument();
    expect(screen.getByText('Finished -')).toBeInTheDocument();
    expect(container.querySelector('.wave-fs-viewer-field')).toBeNull();
    expect(screen.queryByText('accepted')).toBeNull();
  });

  it('throws when required fields are missing', () => {
    expect(() => RunDetailViewer.parse('[{"id":"run_1"}]')).toThrow();
    expect(() => RunDetailViewer.parse('{"status":"running"}')).toThrow();
  });
});
