import { act, cleanup } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import { createIsolatedRetryFixture, mountIsolatedRetryRoute } from './isolated-task-retry-fixture.tsx';
import '../../styles/entry.css';

afterEach(cleanup);

it('recovers a stopped independent attempt and retains its old failure beside the successful result on reload', async () => {
  const fixture = createIsolatedRetryFixture();
  let app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await page.viewport(1280, 1000);
    await page.getByText('Reference', { exact: true }).click();
    await userEvent.click(document.querySelector('[data-nc-task-state] > summary')!);
    await expect.element(page.getByText('Current attempt 1 · Failed', { exact: true })).toBeVisible();
    await expect.element(page.getByText(`Task failed: ${fixture.failureReason}`, { exact: true })).toBeVisible();
    await expect.element(page.getByText(fixture.stoppingReason, { exact: true })).toBeVisible();
    await expect.element(page.getByRole('button', { name: 'Recover task', exact: true })).not.toBeInTheDocument();
    await page.screenshot({ path: '__screenshots__/issue-1501-retry-stopping.png' });
    fixture.stopped();
    await page.getByRole('button', { name: 'Refresh execution history', exact: true }).click();
    await expect.element(page.getByText(fixture.readyReason, { exact: true })).toBeVisible();
    await expect.element(page.getByRole('button', { name: 'Recover task', exact: true })).toBeVisible();
    expect(fixture.requests.filter((request) => request.method === 'POST')).toHaveLength(0);
    await page.screenshot({ path: '__screenshots__/issue-1501-retry-ready.png' });
    await page.getByRole('button', { name: 'Recover task', exact: true }).click();
    await expect.element(page.getByText('Requesting recovery…', { exact: true })).toBeVisible();
    expect(fixture.requests.filter((request) => request.method === 'POST')).toHaveLength(1);
    await act(() => { fixture.acceptRecovery(); return Promise.resolve(); });
    await expect.element(page.getByText('Current attempt 2 · Queued', { exact: true })).toBeVisible();
    fixture.running();
    await page.getByRole('button', { name: 'Refresh execution history', exact: true }).click();
    await expect.element(page.getByText('Current attempt 2 · Running', { exact: true })).toBeVisible();
    fixture.complete();
    await page.getByRole('button', { name: 'Refresh execution history', exact: true }).click();
    await expect.element(page.getByText('Current attempt 2 · Completed', { exact: true })).toBeVisible();
    await expect.element(page.getByText(/"answer": 42/)).toBeVisible();
    await page.getByText('Attempt history (2)', { exact: true }).click();
    await page.getByText('Attempt 1 · Failed', { exact: true }).click();
    await expect.element(page.getByText(`Task failed: ${fixture.failureReason}`, { exact: true })).toBeVisible();
    await expect.element(page.getByText(fixture.goal, { exact: true })).toBeVisible();
    await page.screenshot({ path: '__screenshots__/issue-1501-retry-completed.png' });
    const reportReads = fixture.requests.filter((request) => /\/attempts\/[^/]+\/report$/.test(request.path)).length;
    app.dispose();
    app = mountIsolatedRetryRoute(fixture.transport);
    await page.getByText('Reference', { exact: true }).click();
    await userEvent.click(document.querySelector('[data-nc-task-state] > summary')!);
    await expect.element(page.getByText('Current attempt 2 · Completed', { exact: true })).toBeVisible();
    await expect.element(page.getByText(/"answer": 42/)).toBeVisible();
    await page.getByText('Attempt history (2)', { exact: true }).click();
    await page.getByText('Attempt 1 · Failed', { exact: true }).click();
    await expect.element(page.getByText(`Task failed: ${fixture.failureReason}`, { exact: true })).toBeVisible();
    expect(fixture.requests.filter((request) => /\/attempts\/[^/]+\/report$/.test(request.path)).length).toBeGreaterThan(reportReads);
    expect(fixture.requests.filter((request) => request.method === 'POST')).toHaveLength(1);
    await page.screenshot({ path: '__screenshots__/issue-1501-retry-reloaded.png' });
  } finally { app.dispose(); }
});
