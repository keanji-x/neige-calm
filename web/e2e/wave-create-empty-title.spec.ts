// E2E: create a wave with an empty task description.
//
// Issue #409 makes NewTaskForm's task description optional. This spec
// drives the auto-match branch: seed a cove folder claim, open
// "+ New wave", leave the description blank, type a cwd under the
// claimed folder, submit, and assert the wave detail page renders the
// shared "Untitled wave" fallback while the persisted title remains "".

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

test('creates a new wave with an empty title and renders the fallback label', async ({
  page,
}) => {
  const ts = Date.now();
  const coveName = `E2E empty-title cove ${ts}`;
  const folderPath = `/tmp/playwright-empty-title-${ts}`;
  const cwd = `${folderPath}/worktree`;

  const coveRes = await page.request.post('/api/coves', {
    data: { name: coveName, color: '#5a9' },
    headers: { 'content-type': 'application/json' },
  });
  expect(coveRes.ok()).toBeTruthy();
  const cove = (await coveRes.json()) as { id: string };
  createdCoveIds.push(cove.id);

  const folderRes = await page.request.post(`/api/coves/${cove.id}/folders`, {
    data: { path: folderPath },
    headers: { 'content-type': 'application/json' },
  });
  expect(folderRes.ok()).toBeTruthy();

  await page.goto(`/calm/cove/${cove.id}`);
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  await page.getByRole('button', { name: /new wave/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  await expect(form.getByLabel(/task description/i)).toHaveValue('');
  await form.getByLabel(/working directory/i).fill(cwd);

  const banner = form.getByTestId('cove-auto-match');
  await expect(banner).toBeVisible({ timeout: 5_000 });
  await expect(banner).toContainText(coveName);
  await expect(banner).toContainText(folderPath);

  await form.getByRole('button', { name: 'Create task', exact: true }).click();

  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/, { timeout: 10_000 });
  await expect(page.getByText('Untitled wave', { exact: true }).first()).toBeVisible();

  const waveId = new URL(page.url()).pathname.split('/').pop()!;
  const waveRes = await page.request.get(`/api/waves/${waveId}`);
  expect(waveRes.ok()).toBeTruthy();
  const { wave } = (await waveRes.json()) as {
    wave: { cove_id: string; cwd: string; title: string };
  };
  expect(wave.cove_id).toBe(cove.id);
  expect(wave.cwd).toBe(cwd);
  expect(wave.title).toBe('');
});
