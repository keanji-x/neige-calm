import { expect, test, type Page } from '@playwright/test';
import { createCove } from './helpers/seed.js';

const createdCoveIds: string[] = [];

/* #1209 — the Task field's accessible name. Visually hidden after the astryx
   rewrite (one line, prompt in the placeholder), so the browser-level check
   that it is still *named* is exactly this locator resolving. */
const TASK_LABEL = 'What this wave should do';

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
  await expect(dialog.getByLabel(TASK_LABEL)).toBeVisible();
  // #1147 S3 — the Folder control is present and **optional**. This test walks
  // the default path (nothing picked), which must stay byte-identical to the
  // #1131 body: the kernel keys its managed-workspace branch on the absence of
  // `cwd`, so a control that defaulted to `$HOME` or to `""` would silently
  // move every wave onto the attached branch.
  // #1228 — the control names itself ("Choose a folder"), so there is no outer
  // label to find it by: unset it is a chip whose text, `title` and accessible
  // name are that one sentence.
  await expect(dialog.getByRole('button', { name: 'Choose a folder' })).toBeVisible();
  // #1209 — no template is the default and is left alone here: this case is
  // the pre-template create, and the assertions below say the picker added no
  // field to it. The picker is collapsed, so "what is selected" is read off
  // the trigger's accessible name rather than a checked row — and unset that
  // name is the question, not a choice (#1228).
  await expect(dialog.getByRole('button', { name: 'Choose a template' })).toBeVisible();
  await dialog.getByLabel(TASK_LABEL).fill(title);
  const [createRequest] = await Promise.all([
    page.waitForRequest((pending) => pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/waves'),
    dialog.getByRole('button', { name: 'Create wave' }).click(),
  ]);
  const body = createRequest.postDataJSON() as Record<string, unknown>;
  expect(body).toMatchObject({ cove_id: cove.id, title });
  expect(body).toHaveProperty('theme');
  expect(body).not.toHaveProperty('cwd');
  expect(body).not.toHaveProperty('attach_folder');
  // `toMatchObject` above would not notice these, and no-template must not send
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
  await dialog.getByLabel(TASK_LABEL).fill(title);
  await dialog.getByRole('button', { name: /^Choose a template$|^Template: / }).click();

  /* #1209 — the option says what the template pre-sets, and it says it from
     the kernel's own plan: `small-change` seeds inspect → implement → verify.
     The option itself is the hover trigger now — there is no separate
     "N tasks" label, and therefore no extra tab stop inside the menu.
     Hovering opens the card in a real browser, which is the part jsdom cannot
     prove (the layer is a `popover`, hidden by the UA stylesheet until then). */
  const option = page.getByRole('menuitem', { name: /^Small change/ });
  await expect(option).toBeVisible();
  await expect(page.getByText(/^\d+ tasks?$/)).toHaveCount(0);
  /* The card is addressed through the option, never guessed at by role.
     `getByRole('dialog')` would match two elements here and throw strict mode:
     `HoverCard` renders its layer *inline*, so the card is a descendant of the
     New wave dialog, and Playwright's `hasText` reads `textContent` without
     skipping `display:none` — a closed card's text still counts towards its
     ancestor. `aria-describedby` has no such ambiguity: `HoverCard` writes the
     layer's own id onto its trigger, `DropdownMenuItem` sets no
     `aria-describedby` of its own, so this attribute is exactly one id, and an
     id matches exactly one element.
     `[id="…"]` and not `#…`: the id comes from React's `useId`, which is
     `«r0»`-shaped — an attribute selector does not care.
     Not runnable on the dev box (the stack is docker + a 0-swap prod host);
     this case is verified in CI. */
  await option.hover();
  const cardId = await option.getAttribute('aria-describedby');
  expect(cardId, 'the option must describe its hover card').toBeTruthy();
  const taskCard = page.locator(`[id="${cardId ?? ''}"]`);
  await expect(taskCard).toHaveCount(1);
  await expect(taskCard).toBeVisible();
  await expect(taskCard).toContainText('implement');
  await expect(taskCard).toContainText('verify');
  // Another template's tasks are not in this card.
  await expect(taskCard).not.toContainText('gather-facts');
  await option.click();
  await expect(dialog.getByRole('button', { name: 'Template: Small change' })).toBeVisible();

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
