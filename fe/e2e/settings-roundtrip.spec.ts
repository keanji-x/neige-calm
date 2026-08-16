import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

const createdCoveIds: string[] = [];
let originalHttpProxy: string | null = null;

function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

async function readHttpProxy(request: APIRequestContext): Promise<string | null> {
  const response = await request.get('/api/settings');
  const body = await response.json() as { settings: Record<string, string> };
  return body.settings.http_proxy ?? null;
}

test.beforeEach(async ({ request }) => {
  createdCoveIds.length = 0;
  originalHttpProxy = await readHttpProxy(request);
});
test.afterEach(async ({ request }) => {
  await request.put('/api/settings', { data: { settings: { http_proxy: originalHttpProxy } } });
  for (const id of createdCoveIds) await request.delete(`/api/coves/${id}`);
  createdCoveIds.length = 0;
});

test('persists network and appearance settings across reloads', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const proxy = `http://fe-e2e-${Date.now()}.invalid:3128`;
  await page.goto('/next/settings');
  await page.getByLabel('HTTP proxy').fill(proxy);
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('status')).toHaveText('Saved.');
  expect(await readHttpProxy(request)).toBe(proxy);

  await page.reload();
  await expect(page.getByLabel('HTTP proxy')).toHaveValue(proxy);
  await page.getByRole('radio', { name: 'Dark' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.reload();
  await expect(page.getByRole('radio', { name: 'Dark' })).toHaveAttribute('aria-checked', 'true');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  expect(errors).toEqual([]);
});
