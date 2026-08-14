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
  await page.getByRole('button', { name: 'New wave' }).click();

  const dialog = page.getByRole('dialog', { name: 'New wave' });
  const title = `FE e2e wave ${Date.now()}`;
  const cwd = `/tmp/fe-e2e-${Date.now()}-${cove.id}`;
  await dialog.getByLabel('Task').fill(title);
  await dialog.getByLabel('Working directory').fill(cwd);
  await dialog.getByLabel('Claim this folder for the cove').check();
  await dialog.getByRole('button', { name: 'Create wave' }).click();

  await expect(page).toHaveURL(/\/wave\/[0-9a-f-]+$/i);
  await expect(page.locator('[data-nc-page-title]', { hasText: title })).toBeVisible();
  await expect(page.getByRole('button', { name: new RegExp(`^Wave ${title},`) })).toBeVisible();
  const response = await request.get(`/api/coves/${cove.id}/waves`);
  expect(response.ok()).toBe(true);
  expect(await response.json() as { title: string }[]).toEqual(
    expect.arrayContaining([expect.objectContaining({ title })]),
  );
  expect(errors).toEqual([]);
});
