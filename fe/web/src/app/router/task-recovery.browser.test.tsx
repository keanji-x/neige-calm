import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TaskAttempt, TaskRecoveryView } from '../../../../core/domain/task-recovery.ts';
import { deriveReportTasks, type TrackReport } from '../../../../core/domain/report.ts';
import { useCurrentTaskRows } from './task-execution.ts';
import { ReportDocument } from '../../features/report/document/public.tsx';
import '../../styles/entry.css';
import { TaskRecovery } from './task-recovery.tsx';

afterEach(cleanup);

it('recovers from the task disclosure and navigates prior evidence at desktop and phone widths', async () => {
  const requests: ApiRequest[] = [];
  const first: TaskAttempt = { attempt_id: 'attempt-one', generation: 1, status: 'failed',
    status_detail: 'The implementation did not pass validation.', worker_card_id: 'worker-old',
    created_at_ms: 1788600000000, finished_at_ms: 1788600060000 };
  const second: TaskAttempt = { attempt_id: 'attempt-two', generation: 2, status: 'dispatched',
    status_detail: null, worker_card_id: null, created_at_ms: 1788600070000, finished_at_ms: null };
  let current = first;
  let conversation: string | null = null;
  const transport: ApiTransportPort = { send(request) {
    return Promise.resolve().then(() => {
    requests.push(request);
    if (request.method === 'POST') {
      current = second;
      return { status: 200, statusText: 'OK', body: { key: 'B', previous_attempt_id: first.attempt_id,
        attempt_id: second.attempt_id, generation: 2 } };
    }
    const body: TaskRecoveryView = { key: 'B', current, attempts: current === first ? [first] : [first, second],
      recovery: { allowed: current === first, code: current === first ? 'available' : 'not_failed',
        reason: 'Start a new attempt under the unchanged task requirements.' } };
    return { status: 200, statusText: 'OK', body };
    });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  await page.viewport(1080, 800);
  const report: TrackReport = { summary: '', body: '', blocks: [{ id: 'b-B', kind: 'task', payload: {
    key: 'B', declared_by: 'spec', kind: 'codex', ready: true, goal: 'Finish the calculation under the original requirements.',
  } }] };
  function Document() {
    const rows = useCurrentTaskRows('w1', deriveReportTasks(report.blocks, [
      { blockId: 'b-B', key: 'B', schedulable: true, status: 'failed', workerCardId: 'worker-old' },
    ]));
    return <ReportDocument report={report} taskRows={rows} empty={null}
      renderTaskExecution={(task, expanded) => <TaskRecovery trackId="w1" taskKey={task.key} expanded={expanded}
        transport={transport} unauthorized={unauthorized} openableWorkerIds={new Set(['worker-old'])}
        openWorker={(cardId) => { conversation = cardId; }} />} />;
  }
  render(<QueryClientProvider client={client}><div style={{ padding: 24 }}><Document /></div></QueryClientProvider>);
  await userEvent.click(document.querySelector('[data-nc-report-reference] > summary')!);
  await userEvent.click(document.querySelector('[data-nc-task-state] > summary')!);
  await page.getByRole('button', { name: 'Recover task', exact: true }).click();
  await expect.element(page.getByText('Current attempt 2 · Preparing')).toBeVisible();
  expect(document.querySelector('[data-nc-task-state] > summary')!.textContent).toContain('Preparing');
  expect(document.querySelector('[data-nc-task-state] > summary')!.textContent).not.toContain('failed');
  await page.getByText('Attempt history (2)').click();
  await page.getByText('Attempt 1 · Failed', { exact: true }).click();
  await expect.element(page.getByText('The implementation did not pass validation.')).toBeVisible();
  await page.getByRole('button', { name: 'Open attempt 1' }).click();
  expect(conversation).toBe('worker-old');
  expect(requests.filter((request) => request.method === 'POST')).toHaveLength(1);
  await page.screenshot({ path: '__screenshots__/issue-1501-desktop.png' });
  await page.viewport(390, 844);
  const action = page.getByRole('button', { name: 'Refresh execution history' });
  await expect.element(action).toBeVisible();
  await action.click();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(390);
  await page.screenshot({ path: '__screenshots__/issue-1501-phone.png' });
});
