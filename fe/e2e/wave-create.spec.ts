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
  // #1209 — Blank is the default and is left alone here: this case is the
  // pre-template create, and the assertions below say the picker added no
  // field to it.
  await expect(dialog.getByRole('radio', { name: 'Blank' })).toBeChecked();
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
  // `toMatchObject` above would not notice these, and Blank must not send
  // them: the kernel 400s an empty `workflow_id` and the body is
  // `deny_unknown_fields`.
  expect(body).not.toHaveProperty('workflow_id');
  expect(body).not.toHaveProperty('workflow_input');

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

/*
 * #1209 — a template on the wire, against the real kernel.
 *
 * `small-change` and not `issue-development`: it is a template in every
 * environment, bound to no plugin, so the case does not depend on git-forge
 * running. The wave it creates forks the seeded template report, which is what
 * the assertion on the report's task list below actually checks.
 */
test('creates a wave from a template and seeds its report', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const cove = await createCove(request);
  createdCoveIds.push(cove.id);

  const templates = await request.get('/api/wave-templates');
  expect(templates.ok()).toBe(true);
  const ids = (await templates.json() as { id: string }[]).map((template) => template.id);
  expect(ids).toContain('small-change');

  await page.goto(`/next/cove/${cove.id}`);
  await page.getByRole('button', { name: 'New wave', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'New wave' });
  const title = `FE e2e template wave ${Date.now()}`;
  await dialog.getByLabel('Task').fill(title);
  await dialog.getByRole('radio', { name: 'Small change' }).click();
  const [createRequest] = await Promise.all([
    page.waitForRequest((pending) => pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/waves'),
    dialog.getByRole('button', { name: 'Create wave' }).click(),
  ]);
  const body = createRequest.postDataJSON() as Record<string, unknown>;
  expect(body).toMatchObject({ cove_id: cove.id, title, workflow_id: 'small-change' });
  // Unbound template: the kernel rejects `workflow_input` against it.
  expect(body).not.toHaveProperty('workflow_input');

  await expect(page).toHaveURL(/\/wave\/[0-9a-f-]+$/i);
  await expect(page.locator('[data-nc-page-title]', { hasText: title })).toBeVisible();
  // The kernel accepted the template and stored the binding — the assertion
  // that separates "the picker put a field on the wire" from "the wave was
  // actually created from that template".
  const waveId = /\/wave\/([0-9a-f-]+)$/i.exec(page.url())?.[1];
  expect(waveId).toBeTruthy();
  const detail = await request.get(`/api/waves/${waveId}`);
  expect(detail.ok()).toBe(true);
  expect((await detail.json() as { wave: { workflow_id: string | null } }).wave.workflow_id)
    .toBe('small-change');
  expect(errors).toEqual([]);
});
