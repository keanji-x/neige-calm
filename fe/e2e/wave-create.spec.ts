import { expect, test, type Page } from '@playwright/test';
import { createCove } from './helpers/seed.js';

const createdCoveIds: string[] = [];

/* The words the task field answered to before #1211 S2 deleted it. Kept as
   literals so the absence checks below are about *those strings* and not about
   "some textbox": the dialog still renders one for the issue-development
   template, so a bare textbox count would say nothing. */
const TASK_LABEL = 'What this wave should do';
const TASK_PLACEHOLDER = 'What should this wave do?';

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

/*
 * #1211 S2 — a wave is created without saying anything first.
 *
 * The dialog no longer collects a sentence: the title is not the intent any
 * more, the kernel takes `#[serde(default)]` for it, and the spec agent names
 * the wave through `calm.wave.rename` once it knows what the wave is for —
 * which only works while the stored title is empty. So this case asserts the
 * `title` **key** is absent from the POST rather than asserting a value: an
 * empty string reaches the same stored title but says this client decided the
 * name, and the whole point is that it did not.
 */
test('creates a wave from the cove page with no title, and persists it', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const cove = await createCove(request);
  createdCoveIds.push(cove.id);
  await page.goto(`/next/cove/${cove.id}`);
  // exact: the rail's per-cove `+` is `New wave in …`, a substring match.
  await page.getByRole('button', { name: 'New wave', exact: true }).click();

  const dialog = page.getByRole('dialog', { name: 'New wave' });
  // Gone, by both of the strings it used to answer to.
  await expect(dialog.getByLabel(TASK_LABEL)).toHaveCount(0);
  await expect(dialog.getByPlaceholder(TASK_PLACEHOLDER)).toHaveCount(0);
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
  // Nothing was filled in, and Create is live regardless.
  await expect(dialog.getByRole('button', { name: 'Create wave' })).toBeEnabled();
  const [createRequest] = await Promise.all([
    page.waitForRequest((pending) => pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/waves'),
    dialog.getByRole('button', { name: 'Create wave' }).click(),
  ]);
  const body = createRequest.postDataJSON() as Record<string, unknown>;
  expect(body).toMatchObject({ cove_id: cove.id });
  // Absence, not `title: ''` — see the note above this test.
  expect(body).not.toHaveProperty('title');
  expect(body).toHaveProperty('theme');
  expect(body).not.toHaveProperty('cwd');
  expect(body).not.toHaveProperty('attach_folder');
  // `toMatchObject` above would not notice these, and no-template must not send
  // them: the kernel 400s an empty `template_id` and the body is
  // `deny_unknown_fields`.
  expect(body).not.toHaveProperty('template_id');
  expect(body).not.toHaveProperty('template_input');

  await expect(page).toHaveURL(/\/wave\/[0-9a-f-]+$/i);
  /* The display fallback (#409) is what an unnamed wave reads as — and it is a
     *placeholder* now: the page title shows it while `wave.title` is empty,
     and the rename box opens blank rather than pre-filled with it (#1211 S2). */
  await expect(page.locator('[data-nc-page-title]', { hasText: 'Untitled wave' })).toBeVisible();
  const waveId = /\/wave\/([0-9a-f-]+)$/i.exec(page.url())?.[1];
  expect(waveId).toBeTruthy();
  await page.getByRole('button', { name: 'Rename wave' }).click();
  await expect(page.getByRole('textbox', { name: 'Wave title' })).toHaveValue('');
  const response = await request.get(`/api/coves/${cove.id}/waves`);
  expect(response.ok()).toBe(true);
  expect(await response.json() as { id: string; title: string }[]).toEqual(
    expect.arrayContaining([expect.objectContaining({ id: waveId, title: '' })]),
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
 * running.
 *
 * #1300 — the wave it creates no longer *forks* anything. A template is a
 * read-only recipe instantiated inside the create transaction, not a hidden
 * wave to copy. The assertions below moved with it: they check the report the
 * new wave actually holds, which is what this case's name always promised.
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
  expect(body).toMatchObject({ cove_id: cove.id, template_id: 'small-change' });
  expect(body).not.toHaveProperty('title');
  // Unbound template: the kernel rejects `template_input` against it.
  expect(body).not.toHaveProperty('template_input');

  await expect(page).toHaveURL(/\/wave\/[0-9a-f-]+$/i);
  await expect(page.locator('[data-nc-page-title]', { hasText: 'Untitled wave' })).toBeVisible();
  // The kernel accepted the template and stored the binding — the assertion
  // that separates "the picker put a field on the wire" from "the wave was
  // actually created from that template".
  const waveId = /\/wave\/([0-9a-f-]+)$/i.exec(page.url())?.[1];
  expect(waveId).toBeTruthy();
  const detail = await request.get(`/api/waves/${waveId}`);
  expect(detail.ok()).toBe(true);
  const body = await detail.json() as {
    wave: { template_id: string | null };
    cards: { kind: string; payload: { body?: string } }[];
  };
  expect(body.wave.template_id).toBe('small-change');

  /* #1300 — the assertion this case's name always claimed and never made.
     `template_id` on the wave row says the kernel accepted the binding; it says
     nothing about the report, which is the thing "seeds its report" is about.
     Before #1300 the report came from forking a hidden system-cove wave the
     kernel lazily seeded; it now comes from instantiating a Rust constant in
     the create transaction. Both produce the same document — that equivalence
     is pinned in-process by
     `wave_template_waves.rs::creating_from_a_template_instantiates_its_recipe`
     — and this is the live-stack half: through the real kernel, over HTTP, on
     the wave the browser actually created.

     `ready: false` is asserted alongside the keys because it is the difference
     between "the plan is present" and "the plan is running". A template's tasks
     are pre-set, not released; an instantiation that shipped them ready would
     start dispatching work nobody approved. */
  const report = body.cards.find((card) => card.kind === 'wave-report');
  expect(report, 'the created wave must have a wave-report card').toBeTruthy();
  const reportBody = report?.payload.body ?? '';
  for (const key of ['inspect', 'implement', 'verify']) {
    expect(reportBody, `small-change must pre-set the ${key} task`).toContain(`"key": "${key}"`);
  }
  expect(reportBody).toContain('"ready": false');
  expect(reportBody).not.toContain('"ready": true');

  expect(errors).toEqual([]);
});
