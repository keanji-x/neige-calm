import { test, expect } from '@playwright/test';

const createdCoveIds: string[] = [];

test.beforeEach(() => {
  createdCoveIds.length = 0;
});

test.afterEach(async ({ request }) => {
  for (const id of createdCoveIds) {
    const res = await request.delete(`/api/coves/${id}`);
    if (!res.ok() && res.status() !== 404) {
      throw new Error(
        `cleanup: DELETE /api/coves/${id} -> ${res.status()} ${res.statusText()}`,
      );
    }
  }
  createdCoveIds.length = 0;
});

// PR-A of #594 hides the legacy wave-report card from worker grid/list views
// and moves the first-class Files rail onto WaveReportPage as a placeholder.
// TODO(#594 PR-B): rewrite this as an equivalent report-page assertion using
// the real WaveFileTree, then remove the .skip.
test.skip('Report card file sidebar opens a real wave file', async ({ page }) => {
  const ts = Date.now();
  const coveRes = await page.request.post('/api/coves', {
    data: { name: `E2E report files ${ts}`, color: '#4a8' },
    headers: { 'content-type': 'application/json' },
  });
  expect(coveRes.ok()).toBeTruthy();
  const cove = (await coveRes.json()) as { id: string };
  createdCoveIds.push(cove.id);

  const waveTitle = `E2E report file wave ${ts}`;
  const waveRes = await page.request.post('/api/waves', {
    data: {
      cove_id: cove.id,
      title: waveTitle,
      cwd: `/tmp/playwright-report-files-${ts}`,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!waveRes.ok()) {
    const body = await waveRes.text().catch(() => '<unreadable>');
    throw new Error(
      `POST /api/waves -> ${waveRes.status()} ${waveRes.statusText()}: ${body}`,
    );
  }
  const wave = (await waveRes.json()) as { id: string };

  await page.goto(`/calm/wave/${wave.id}`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);
  await expect(page.getByText(waveTitle, { exact: false }).first()).toBeVisible();

  const reportCard = page.locator('.wave-report-card').first();
  await expect(reportCard).toBeVisible();
  await expect(reportCard.locator('.wave-report-files-tree')).toBeVisible();

  await reportCard.getByRole('treeitem', { name: /cards\// }).click();
  await reportCard.getByRole('treeitem', { name: /index\.json/ }).click();

  const viewer = reportCard.locator('.wave-report-files-code-wrap');
  await expect(viewer).toBeVisible();
  await expect(viewer).toContainText('"id"', { timeout: 5_000 });
});
