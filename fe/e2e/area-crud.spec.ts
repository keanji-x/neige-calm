import { expect, test, type Page } from '@playwright/test';
import { resolve } from 'node:path';

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

test('creates, edits, and deletes an area through the shared dialog', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const name = `FE e2e CRUD ${Date.now()}`;
  const renamed = `${name} edited`;
  await page.goto('/next/');
  const rail = page.locator('nav[aria-label="Workspace"]');
  await rail.getByRole('button', { name: 'New area' }).click();
  const create = page.getByRole('dialog', { name: 'New area' });
  await create.getByRole('textbox', { name: /^Name/ }).fill(name);
  await expect(create.getByRole('button', { name: /template|isolation/i })).toHaveCount(0);
  await create.getByRole('button', { name: 'Create area' }).click();

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
  expect(area).toMatchObject({ default_template_id: null, default_cwd: null });

  // Existing preferences remain stored even though the editor no longer exposes them.
  expect((await request.patch(`/api/areas/${area!.id}`, { data: { default_template_id: 'small-change' } })).ok()).toBe(true);
  await page.reload();

  await row.hover();
  await rail.getByRole('button', { name: `Area actions for ${name}` }).click();
  await page.getByRole('menuitem', { name: 'Edit area' }).click();
  const edit = page.getByRole('dialog', { name: `Edit ${name}` });
  await expect(edit.getByRole('button', { name: /template|isolation/i })).toHaveCount(0);
  await edit.getByRole('textbox', { name: /^Name/ }).fill(renamed);
  await edit.getByRole('button', { name: 'Save changes' }).click();
  const renamedRow = rail.getByRole('button', { name: `Collapse area ${renamed}` });
  await expect(renamedRow).toBeVisible();
  await expect.poll(async () => {
    const response = await request.get('/api/areas');
    return (await response.json() as {
      id: string; name: string; default_template_id: string | null;
    }[]).find((item) => item.id === area!.id);
  }).toMatchObject({ name: renamed, default_template_id: 'small-change' });

  await renamedRow.hover();
  await rail.getByRole('button', { name: `Area actions for ${renamed}` }).click();
  await page.getByRole('menuitem', { name: 'Delete area' }).click();
  const dialog = page.getByRole('dialog', { name: `Delete ${renamed}?` });
  await dialog.getByLabel(`Type ${renamed} to confirm.`).fill(renamed);
  await dialog.getByRole('button', { name: 'Delete area', exact: true }).click();
  await expect(renamedRow).toHaveCount(0);
  await expect.poll(async () => {
    const response = await request.get('/api/areas');
    return (await response.json() as { id: string }[]).some((item) => item.id === area!.id);
  }).toBe(false);
  createdAreaIds.length = 0;
  expect(errors).toEqual([]);
});


test('creates an area with an explicit directory and no template', async ({ page, request }) => {
  const name = `FE e2e directory Area ${Date.now()}`;
  await page.goto('/next/');
  await page.locator('nav[aria-label="Workspace"]').getByRole('button', { name: 'New area' }).click();
  const dialog = page.getByRole('dialog', { name: 'New area' });
  await dialog.getByRole('textbox', { name: /^Name/ }).fill(name);
  await dialog.getByRole('button', { name: 'Default folder: Neige workspace' }).click();
  const browser = page.getByRole('dialog', { name: 'Choose a directory' });
  const select = browser.getByRole('button', { name: 'Select this directory' });
  await expect(select).toBeEnabled();
  const path = browser.getByRole('combobox', { name: 'Directory path' });
  await path.fill(resolve(process.cwd(), '..'));
  const [listing] = await Promise.all([
    page.waitForResponse((response) => new URL(response.url()).pathname === '/api/fs/listdir'),
    path.press('Enter'),
  ]);
  const { path: directory } = await listing.json() as { path: string };
  await expect(select).toBeEnabled();
  await select.click();
  await expect(dialog.getByRole('button', { name: `Default folder: ${directory}` })).toBeVisible();
  await dialog.getByRole('button', { name: 'Create area' }).click();
  await expect(dialog).toHaveCount(0);
  const areas = await (await request.get('/api/areas')).json() as { id: string; name: string }[];
  const area = areas.find((item) => item.name === name);
  expect(area).toBeTruthy();
  createdAreaIds.push(area!.id);
  expect(area).toMatchObject({ default_template_id: null, default_cwd: directory });
});

test('corrects a first Area create 413 with a new identity and body', async ({ page }) => {
  const name = `FE e2e rejected Area ${Date.now()}`;
  const submitted: { key: string | undefined; body: unknown }[] = [];
  await page.route('**/api/areas', async (route) => {
    if (route.request().method() !== 'POST') { await route.continue(); return; }
    submitted.push({ key: route.request().headers()['idempotency-key'], body: route.request().postDataJSON() as unknown });
    if (submitted.length === 1) {
      await route.fulfill({ status: 413, contentType: 'text/plain', body: 'Payload Too Large' });
      return;
    }
    const response = await route.fetch();
    expect(response.status()).toBe(201);
    createdAreaIds.push((await response.json() as { id: string }).id);
    await route.fulfill({ response });
  });
  await page.goto('/next/');
  await page.locator('nav[aria-label="Workspace"]').getByRole('button', { name: 'New area' }).click();
  const dialog = page.getByRole('dialog', { name: 'New area' });
  const input = dialog.getByRole('textbox', { name: /^Name/ });
  await input.fill(name);
  await dialog.getByRole('button', { name: 'Create area' }).click();
  await expect(dialog.getByRole('alert')).toContainText('Payload Too Large');
  await expect(dialog.getByRole('alert')).not.toContainText('Creation could not be confirmed');
  await expect(dialog.getByRole('button', { name: 'Try again' })).toHaveCount(0);
  await expect(input).toBeEnabled();
  await input.fill(`${name} corrected`);
  await dialog.getByRole('button', { name: 'Create area' }).click();
  await expect(dialog).toHaveCount(0);
  expect(submitted).toHaveLength(2);
  expect(submitted[0]?.key).toBeTruthy();
  expect(submitted[1]?.key).toBeTruthy();
  expect(submitted[1]?.key).not.toBe(submitted[0]?.key);
  expect(submitted.map((write) => write.body)).toEqual([
    expect.objectContaining({ name }), expect.objectContaining({ name: `${name} corrected` }),
  ]);
});

test('recovers a committed Area after a lost response and retry 413 without creating a duplicate', async ({ page, request }) => {
  const name = `FE e2e lost Area confirmation ${Date.now()}`;
  const submitted: { key: string | undefined; body: unknown }[] = [];
  await page.route('**/api/areas', async (route) => {
    if (route.request().method() !== 'POST') { await route.continue(); return; }
    submitted.push({
      key: route.request().headers()['idempotency-key'],
      body: route.request().postDataJSON() as unknown,
    });
    if (submitted.length === 2) {
      await route.fulfill({ status: 413, contentType: 'text/plain', body: 'Payload Too Large' });
      return;
    }
    const response = await route.fetch();
    expect(response.status()).toBe(201);
    const area = await response.json() as { id: string };
    if (!createdAreaIds.includes(area.id)) createdAreaIds.push(area.id);
    if (submitted.length === 1) await route.abort('connectionreset');
    else await route.fulfill({ response });
  });
  await page.goto('/next/');
  const rail = page.locator('nav[aria-label="Workspace"]');
  await rail.getByRole('button', { name: 'New area' }).click();
  const dialog = page.getByRole('dialog', { name: 'New area' });
  await dialog.getByRole('textbox', { name: /^Name/ }).fill(name);
  await dialog.getByRole('button', { name: 'Create area' }).click();
  await expect(dialog.getByRole('alert')).toContainText('Creation could not be confirmed');
  await expect(dialog.getByRole('textbox', { name: /^Name/ })).toBeDisabled();
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await rail.getByRole('button', { name: 'New area' }).click();
  await expect(dialog.getByRole('textbox', { name: /^Name/ })).toHaveValue(name);
  await dialog.getByRole('button', { name: 'Try again' }).click();
  await expect(dialog.getByRole('alert')).toContainText('Payload Too Large');
  await expect(dialog.getByRole('alert')).toContainText('Creation could not be confirmed');
  await expect(dialog.getByRole('textbox', { name: /^Name/ })).toBeDisabled();
  await dialog.getByRole('button', { name: 'Try again' }).click();
  await expect(dialog).toHaveCount(0);
  expect(submitted).toHaveLength(3);
  expect(submitted[0]?.key).toBeTruthy();
  expect(submitted[0]?.body).toMatchObject({ default_cwd: null });
  expect(submitted[0]?.body).not.toHaveProperty('default_template_id');
  expect(submitted[1]).toEqual(submitted[0]);
  expect(submitted[2]).toEqual(submitted[0]);
  const areas = await (await request.get('/api/areas')).json() as { id: string; name: string }[];
  expect(areas.filter((area) => area.name === name).map((area) => area.id)).toEqual(createdAreaIds);
  expect(createdAreaIds).toHaveLength(1);
});
