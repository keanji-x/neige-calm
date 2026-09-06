import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TaskAttempt } from '../../../../core/domain/task-recovery.ts';
import { TaskRecovery } from './task-recovery.tsx';

afterEach(() => { cleanup(); vi.useRealTimers(); });

function setup(status: string, older = false) {
  let currentStatus = status;
  let accepted: unknown = null;
  const reads: string[] = [];
  let holdReport = false;
  let releaseReport: (() => void) | undefined;
  const attempt = (attemptId: string, generation: number, state: string): TaskAttempt => ({
    attempt_id: attemptId, generation, status: state, status_detail: state === 'failed' ? 'Original failure detail.' : null,
    blocking_reason: null, worker_card_id: null, created_at_ms: 1000, finished_at_ms: state === 'running' ? null : 2000,
  });
  const transport: ApiTransportPort = { send: (request) => {
    if (request.path.endsWith('/report')) {
      const id = request.path.split('/').at(-2)!;
      reads.push(id);
      const body = { attemptId: id, report: accepted };
      return (holdReport ? new Promise<void>((resolve) => { releaseReport = resolve; }) : Promise.resolve())
        .then(() => ({ status: 200, statusText: 'OK', body }));
    }
    const current = attempt('current-attempt', older ? 2 : 1, currentStatus);
    return Promise.resolve({ status: 200, statusText: 'OK', body: {
      key: 'task', current, attempts: older ? [attempt('old-attempt', 1, 'failed'), current] : [current],
      recovery: { allowed: false, code: 'unavailable', reason: 'Existing recovery explanation.' },
    } });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  render(<QueryClientProvider client={client}><TaskRecovery trackId="track" taskKey="task" expanded
    transport={transport} unauthorized={unauthorized} openWorker={() => undefined} openableWorkerIds={new Set()} /></QueryClientProvider>);
  return { reads, hold: () => { holdReport = true; }, release: () => releaseReport?.(), status: (value: string) => { currentStatus = value; }, report: (value: unknown) => { accepted = value; } };
}

it.each(['done', 'failed', 'canceled'])('does not poll a %s attempt without a native report and retains explicit refresh', async (status) => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const fixture = setup(status);
  await waitFor(() => expect(fixture.reads).toHaveLength(1));
  await act(() => vi.advanceTimersByTimeAsync(12_000));
  expect(fixture.reads).toHaveLength(1);
  expect(screen.getByText('This attempt ended without an accepted report.')).toBeTruthy();
  expect(screen.queryByText('No accepted report yet.')).toBeNull();
  if (status === 'failed') {
    expect(screen.getByText('Current attempt 1 · Failed')).toBeTruthy();
    expect(screen.getByText('Original failure detail.', { selector: 'section[aria-label="Task execution"] > p' })).toBeTruthy();
  }
  fixture.report({ kind: 'completed', result: 'Result found by explicit refresh.', artifacts: [] });
  fireEvent.click(screen.getByRole('button', { name: 'Refresh accepted report' }));
  await screen.findByText('Result found by explicit refresh.');
  expect(fixture.reads).toHaveLength(2);
});

it('stops report polling when running becomes terminal while keeping older terminal reports bounded', async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const fixture = setup('running', true);
  await waitFor(() => expect(fixture.reads).toHaveLength(2));
  await act(() => vi.advanceTimersByTimeAsync(3_000));
  expect(fixture.reads.filter((id) => id === 'current-attempt').length).toBeGreaterThan(1);
  expect(fixture.reads.filter((id) => id === 'old-attempt')).toHaveLength(1);
  fixture.status('done');
  fireEvent.click(screen.getByRole('button', { name: 'Refresh execution history' }));
  await screen.findByText('Current attempt 2 · Completed');
  const count = fixture.reads.length;
  await act(() => vi.advanceTimersByTimeAsync(12_000));
  expect(fixture.reads).toHaveLength(count);
  expect(screen.queryByText('No accepted report yet.')).toBeNull();
});

it.each(['done', 'failed', 'canceled'])('reads the final report once on running → %s before showing final absence', async (status) => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const fixture = setup('running');
  await screen.findByText('No accepted report yet.');
  fixture.status(status);
  fixture.report(status === 'done' ? { kind: 'completed', result: 'The durable final answer.', artifacts: [] }
    : status === 'failed' ? { kind: 'failed', reason: 'The durable failure reason.' } : null);
  fixture.hold();
  fireEvent.click(screen.getByRole('button', { name: 'Refresh execution history' }));
  await waitFor(() => expect(fixture.reads).toHaveLength(2));
  expect(screen.queryByText('This attempt ended without an accepted report.')).toBeNull();
  await act(async () => { fixture.release(); await Promise.resolve(); });
  await screen.findByText(status === 'done' ? 'The durable final answer.'
    : status === 'failed' ? 'Task failed: The durable failure reason.' : 'This attempt ended without an accepted report.');
  await act(() => vi.advanceTimersByTimeAsync(12_000));
  expect(fixture.reads).toHaveLength(2);
});
