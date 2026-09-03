import { expect, test, type Page } from '@playwright/test';

const createdAreaIds: string[] = [];

function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

test.beforeEach(() => { createdAreaIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdAreaIds) await request.delete(`/api/areas/${id}`);
  createdAreaIds.length = 0;
});

test('creates and deletes an area through the UI and persists both changes', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const name = `FE e2e CRUD ${Date.now()}`;
  await page.goto('/next/');
  const rail = page.locator('nav[aria-label="Workspace"]');
  await rail.getByRole('button', { name: 'New area' }).click();
  await rail.getByRole('textbox', { name: 'Area name' }).fill(name);
  await rail.getByRole('textbox', { name: 'Area name' }).press('Enter');

  const row = rail.getByRole('button', { name: `Collapse area ${name}` });
  await expect(row).toBeVisible();
  await expect.poll(async () => {
    const response = await request.get('/api/areas');
    return (await response.json() as { id: string; name: string }[]).find((area) => area.name === name);
  }).toBeTruthy();
  const areasResponse = await request.get('/api/areas');
  const area = (await areasResponse.json() as { id: string; name: string }[]).find((item) => item.name === name);
  expect(area).toBeTruthy();
  createdAreaIds.push(area!.id);

  await rail.getByRole('button', { name: `Area actions for ${name}` }).click();
  await rail.getByRole('menuitem', { name: 'Delete area' }).click();
  const dialog = page.getByRole('dialog', { name: `Delete ${name}?` });
  await dialog.getByLabel(`Type ${name} to confirm.`).fill(name);
  await dialog.getByRole('button', { name: 'Delete area', exact: true }).click();
  await expect(row).toHaveCount(0);
  await expect.poll(async () => {
    const response = await request.get('/api/areas');
    return (await response.json() as { id: string }[]).some((item) => item.id === area!.id);
  }).toBe(false);
  createdAreaIds.length = 0;
  expect(errors).toEqual([]);
});
