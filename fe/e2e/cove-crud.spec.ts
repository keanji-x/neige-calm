import { expect, test, type Page } from '@playwright/test';

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

test('creates and deletes a cove through the UI and persists both changes', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const name = `FE e2e CRUD ${Date.now()}`;
  await page.goto('/next/');
  const rail = page.locator('nav[aria-label="Workspace"]');
  await rail.getByRole('button', { name: 'New area' }).click();
  await rail.getByRole('textbox', { name: 'Area name' }).fill(name);
  await rail.getByRole('textbox', { name: 'Area name' }).press('Enter');

  const row = rail.getByRole('button', { name, exact: true });
  await expect(row).toBeVisible();
  await expect.poll(async () => {
    const response = await request.get('/api/coves');
    return (await response.json() as { id: string; name: string }[]).find((cove) => cove.name === name);
  }).toBeTruthy();
  const covesResponse = await request.get('/api/coves');
  const cove = (await covesResponse.json() as { id: string; name: string }[]).find((item) => item.name === name);
  expect(cove).toBeTruthy();
  createdCoveIds.push(cove!.id);

  await row.click();
  await expect(page).toHaveURL(new RegExp(`/cove/${cove!.id}$`));
  await rail.getByRole('button', { name: `Delete area ${name}` }).click();
  const dialog = page.getByRole('dialog', { name: `Delete ${name}?` });
  await dialog.getByLabel(`Type ${name} to confirm.`).fill(name);
  await dialog.getByRole('button', { name: 'Delete area', exact: true }).click();
  await expect(row).toHaveCount(0);
  await expect.poll(async () => {
    const response = await request.get('/api/coves');
    return (await response.json() as { id: string }[]).some((item) => item.id === cove!.id);
  }).toBe(false);
  createdCoveIds.length = 0;
  expect(errors).toEqual([]);
});
