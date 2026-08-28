import { expect, test, type Page } from '@playwright/test';
import { createCove } from './helpers/seed.js';

const createdCoveIds: string[] = [];

function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

test.beforeEach(() => { createdCoveIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdCoveIds) await request.delete(`/api/coves/${id}`);
  createdCoveIds.length = 0;
});

test('creates a wave from the cove page and persists it', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const cove = await createCove(request);
  createdCoveIds.push(cove.id);
  await page.goto(`/next/cove/${cove.id}`);
  // exact: the rail's per-cove `+` is `New wave in …`, a substring match.
  await page.getByRole('button', { name: 'New wave', exact: true }).click();

  const dialog = page.getByRole('dialog', { name: 'New wave' });
  const title = `FE e2e wave ${Date.now()}`;
  await expect(dialog.getByLabel('Task')).toBeVisible();
  await expect(dialog.getByLabel('Folder')).toHaveCount(0);
  await dialog.getByLabel('Task').fill(title);
  const [createRequest] = await Promise.all([
    page.waitForRequest((pending) => pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/waves'),
    dialog.getByRole('button', { name: 'Create wave' }).click(),
  ]);
  const body = createRequest.postDataJSON() as Record<string, unknown>;
  expect(body).toMatchObject({ cove_id: cove.id, title });
  expect(body).toHaveProperty('theme');
  expect(body).not.toHaveProperty('cwd');
  expect(body).not.toHaveProperty('attach_folder');

  await expect(page).toHaveURL(/\/wave\/[0-9a-f-]+$/i);
  await expect(page.locator('[data-nc-page-title]', { hasText: title })).toBeVisible();
  await expect(page.getByRole('button', { name: new RegExp(`^Wave ${title},`) })).toBeVisible();
  const response = await request.get(`/api/coves/${cove.id}/waves`);
  expect(response.ok()).toBe(true);
  expect(await response.json() as { title: string }[]).toEqual(
    expect.arrayContaining([expect.objectContaining({ title })]),
  );
  const foldersResponse = await request.get(`/api/coves/${cove.id}/folders`);
  expect(foldersResponse.ok()).toBe(true);
  expect(await foldersResponse.json()).toEqual([]);
  expect(errors).toEqual([]);
});
