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
  // There is no Save button: leaving the field is the commit. The confirmation
  // is that row's own live region — **scoped to the row**, because every proxy
  // row mounts one of these empty and keeps it mounted (a region that arrives
  // together with its text is commonly not announced at all). Two rows, two
  // regions: an unscoped `getByRole('status')` matches both.
  await page.getByLabel('HTTP proxy').blur();
  const httpRow = page.getByRole('listitem').filter({ has: page.getByLabel('HTTP proxy') });
  await expect(httpRow.getByRole('status')).toContainText('Saved.');
  expect(await readHttpProxy(request)).toBe(proxy);

  await page.reload();
  await expect(page.getByLabel('HTTP proxy')).toHaveValue(proxy);

  // Appearance is its own section now, and theme is a dropdown rather than a
  // segmented control: it states the current value and lists the rest on ask.
  await page.goto('/next/settings/appearance');
  await page.getByRole('combobox', { name: 'Theme' }).click();
  await page.getByRole('option', { name: 'Dark' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.reload();
  await expect(page.getByRole('combobox', { name: 'Theme' })).toContainText('Dark');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  expect(errors).toEqual([]);
});
