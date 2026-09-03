// E2E: create a wave with an empty task description.
//
// Issue #409 makes NewTaskForm's task description optional. This spec
// drives the auto-match branch: seed an area folder claim, open
// "+ New wave", leave the description blank, type a cwd under the
// claimed folder, submit, and assert the wave detail page renders the
// shared "Untitled wave" fallback while the persisted title remains "".

import { test, expect } from '@playwright/test';
import {
  attachedWorkspacePath,
  cleanupAttachedWorkspaces,
  createGitWorkTree,
  createWorkTreeSubdir,
} from './helpers/attached-workspace';

const createdAreaIds: string[] = [];

test.beforeEach(() => {
  createdAreaIds.length = 0;
});

test.afterEach(async ({ request }) => {
  for (const id of createdAreaIds) {
    const res = await request.delete(`/api/areas/${id}`);
    if (!res.ok() && res.status() !== 404) {
      throw new Error(
        `cleanup: DELETE /api/areas/${id} -> ${res.status()} ${res.statusText()}`,
      );
    }
  }
  createdAreaIds.length = 0;
  cleanupAttachedWorkspaces();
});

test('creates a new wave with an empty title and renders the fallback label', async ({
  page,
}) => {
  const ts = Date.now();
  const areaName = `E2E empty-title area ${ts}`;
  // #1147 S3 — the submitted cwd is an *attached* workspace, so it has
  // to exist and sit inside a Git work tree that the kernel can see.
  // `folderPath` is the work-tree root (and the area's folder claim);
  // `cwd` is a real directory beneath it, which is what makes the
  // auto-match banner fire on `folderPath`. See
  // `helpers/attached-workspace.ts` for why these live under `$HOME`.
  const folderPath = createGitWorkTree(
    attachedWorkspacePath(`neige-e2e-empty-title-${ts}`),
  );
  const cwd = createWorkTreeSubdir(folderPath, 'worktree');

  const areaRes = await page.request.post('/api/areas', {
    data: { name: areaName, color: '#5a9' },
    headers: { 'content-type': 'application/json' },
  });
  expect(areaRes.ok()).toBeTruthy();
  const area = (await areaRes.json()) as { id: string };
  createdAreaIds.push(area.id);

  const folderRes = await page.request.post(`/api/areas/${area.id}/folders`, {
    data: { path: folderPath },
    headers: { 'content-type': 'application/json' },
  });
  expect(folderRes.ok()).toBeTruthy();

  await page.goto(`/calm/area/${area.id}`);
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);

  await page.getByRole('button', { name: /new wave/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  await expect(form.getByLabel(/task description/i)).toHaveValue('');
  await form.getByLabel(/working directory/i).fill(cwd);

  const banner = form.getByTestId('area-auto-match');
  await expect(banner).toBeVisible({ timeout: 5_000 });
  await expect(banner).toContainText(areaName);
  await expect(banner).toContainText(folderPath);

  await form.getByRole('button', { name: 'Create task', exact: true }).click();

  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/, { timeout: 10_000 });
  await expect(page.getByText('Untitled wave', { exact: true }).first()).toBeVisible();

  const waveId = new URL(page.url()).pathname.split('/').pop()!;
  const waveRes = await page.request.get(`/api/waves/${waveId}`);
  expect(waveRes.ok()).toBeTruthy();
  const { wave } = (await waveRes.json()) as {
    wave: { area_id: string; cwd: string; title: string };
  };
  expect(wave.area_id).toBe(area.id);
  expect(wave.cwd).toBe(cwd);
  expect(wave.title).toBe('');
});
