import { expect, test, type Page } from '@playwright/test';
import { createArea } from './helpers/seed.js';

const createdAreaIds: string[] = [];

/* The two strings the composer's task field answers to. astryx puts `label` on
   the `contenteditable` as `aria-label`, so the browser-level check that the
   field is still *named* is exactly this locator resolving; the placeholder is
   the empty-state prompt beside it. Kept as literals so the checks below are
   about *those strings* and not about "some textbox" (#1211 S2 deleted the
   title field, not this one — the sentence is the track's intent now). */
const TASK_LABEL = 'What this track should do';
const TASK_PLACEHOLDER = 'What should this track do?';

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

/*
 * #1211 — a track is created without *naming* it first.
 *
 * Nothing here collects a title any more: the title is not the intent (S2), the
 * kernel takes `#[serde(default)]` for it, and the planner agent names the track
 * through `calm.track.rename` once it knows what the track is for — which only
 * works while the stored title is empty. So this case asserts the `title`
 * **key** is absent from the POST rather than asserting a value: an empty
 * string reaches the same stored title but says this client decided the name,
 * and the whole point is that it did not.
 *
 * What the reader does type is the track's *intent*, into the composer this page
 * became (S3). It is not delivered yet — the absence is asserted at the end of
 * this case, under #1299.
 */
test('creates a track from the area page with no title, and persists it', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  await page.goto(`/next/area/${area.id}`);
  // exact: the rail's per-area `+` is `New track in …`, a substring match.
  await page.getByRole('button', { name: 'New track', exact: true }).click();

  /* #1211 — a route, not a modal. `waitForURL` is the surface being ready;
     the composer being visible is the surface being *usable*, and both are
     asserted because a route that renders an error box would satisfy only the
     first. The dialog count is the negative half of the same statement: main's
     `getByRole('dialog', { name: 'New track' })` scope has no successor here. */
  await page.waitForURL(/\/area\/[^/]+\/new$/);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  const message = `FE e2e track ${Date.now()}`;
  await expect(page.getByLabel(TASK_LABEL)).toBeVisible();
  // The empty-state prompt, by the other string the field answers to.
  await expect(page.getByText(TASK_PLACEHOLDER)).toBeVisible();
  // #1147 S3 — the Folder control is present and **optional**. This test walks
  // the default path (nothing picked), which must stay byte-identical to the
  // #1131 body: the kernel keys its managed-workspace branch on the absence of
  // `cwd`, so a control that defaulted to `$HOME` or to `""` would silently
  // move every track onto the attached branch.
  // #1228/#1211 — the control names itself, so there is no outer label to find
  // it by. Unset, its text is the **default** it is holding ("Neige workspace")
  // and its accessible name says which control that is; they are two different
  // strings on purpose.
  await expect(page.getByRole('button', { name: 'Folder: Neige workspace' })).toBeVisible();
  // #1209 — no template is the default and is left alone here: this case is
  // the pre-template create, and the assertions below say the picker added no
  // field to it. The picker is collapsed, so "what is selected" is read off the
  // trigger's accessible name rather than a checked row — and since #1211 that
  // name states the default rather than asking a question.
  await expect(page.getByRole('button', { name: 'Template: No template' })).toBeVisible();
  /* Create is gated on the sentence, which is where #1211 S3 differs from S2:
     the composer *is* the page, so submitting an empty one would create a track
     with nothing in it and nothing on screen to say why it was allowed. S2's
     "Create is live with nothing typed" belonged to a dialog that collected no
     sentence at all. */
  await expect(page.getByRole('button', { name: 'Create track' })).toBeDisabled();
  await page.getByLabel(TASK_LABEL).fill(message);
  await expect(page.getByRole('button', { name: 'Create track' })).toBeEnabled();
  const [createRequest] = await Promise.all([
    page.waitForRequest((pending) => pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/tracks'),
    page.getByRole('button', { name: 'Create track' }).click(),
  ]);
  const body = createRequest.postDataJSON() as Record<string, unknown>;
  expect(body).toMatchObject({ area_id: area.id });
  expect(body).toHaveProperty('theme');
  /* #1211 — the sentence is the track's intent, not its name. The kernel stores
     the empty string and the planner agent renames later via `calm.track.rename`. */
  expect(body).not.toHaveProperty('title');
  expect(body).not.toHaveProperty('cwd');
  expect(body).not.toHaveProperty('attach_folder');
  // `toMatchObject` above would not notice these, and no-template must not send
  // them: the kernel 400s an empty `template_id` and the body is
  // `deny_unknown_fields`.
  expect(body).not.toHaveProperty('template_id');
  expect(body).not.toHaveProperty('template_input');

  await expect(page).toHaveURL(/\/track\/[0-9a-f-]+$/i);
  /* Untitled is the *normal* landing state now, so the page title is the
     display fallback (#409) rather than anything the reader typed. Asserted
     because "the header renders something readable for a blank title" is
     exactly what stops being exercised once the title field is gone — and it
     is a *placeholder*, so the rename box opens blank rather than pre-filled
     with it (#1211 S2). */
  await expect(page.locator('[data-nc-page-title]')).toHaveText(/Untitled track/);
  const trackId = /\/track\/([0-9a-f-]+)$/i.exec(page.url())?.[1];
  expect(trackId).toBeTruthy();
  await page.getByRole('button', { name: 'Rename track' }).click();
  await expect(page.getByRole('textbox', { name: 'Track title' })).toHaveValue('');
  const response = await request.get(`/api/areas/${area.id}/tracks`);
  expect(response.ok()).toBe(true);
  expect(await response.json() as { id: string; title: string }[]).toEqual(
    expect.arrayContaining([expect.objectContaining({ id: trackId, title: '' })]),
  );
  /*
   * #1299 — the sentence is deliberately NOT delivered from this page yet, so
   * this asserts the *absence*: the track carries a planner card and that card has
   * no user message on it. Written as an assertion rather than left out, because
   * "we do not do this yet" is a property worth failing on if someone re-adds
   * the three-write sequence here instead of moving it into the create.
   */
  const detail = await request.get(`/api/tracks/${trackId ?? ''}`);
  expect(detail.ok()).toBe(true);
  const cards = (await detail.json() as { cards: { id: string; kind: string; payload: unknown }[] }).cards;
  const plannerCard = cards.find((card) => card.kind === 'codex'
    && typeof card.payload === 'object' && card.payload !== null
    && (card.payload as { planner_harness?: unknown }).planner_harness === true);
  expect(plannerCard, 'the created track must carry a planner card').toBeTruthy();
  const items = await request.get(`/api/cards/${plannerCard?.id ?? ''}/harness/items?after_id=0&limit=50&direction=asc`);
  expect(items.ok()).toBe(true);
  const params = (await items.json() as { params?: unknown }[])
    .map((item) => (typeof item.params === 'string' ? item.params : '')).join('\n');
  expect(params).not.toContain(message);

  const foldersResponse = await request.get(`/api/areas/${area.id}/folders`);
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
 * #1300 — the track it creates no longer *forks* anything. A template is a
 * read-only recipe instantiated inside the create transaction, not a hidden
 * track to copy. The assertions below moved with it: they check the report the
 * new track actually holds, which is what this case's name always promised.
 */
test('creates a track from a template and seeds its report', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const area = await createArea(request);
  createdAreaIds.push(area.id);

  const templates = await request.get('/api/track-templates');
  expect(templates.ok()).toBe(true);
  const ids = (await templates.json() as { id: string }[]).map((template) => template.id);
  expect(ids).toContain('small-change');

  await page.goto(`/next/area/${area.id}`);
  await page.getByRole('button', { name: 'New track', exact: true }).click();
  await page.waitForURL(/\/area\/[^/]+\/new$/);
  const message = `FE e2e template track ${Date.now()}`;
  await page.getByLabel(TASK_LABEL).fill(message);
  await page.getByRole('button', { name: /^Template: / }).click();

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
     `getByRole('dialog')` is the wrong handle even now that the surface is a
     page: `HoverCard` renders its layer *inline* with `role="dialog"`, and
     Playwright's `hasText` reads `textContent` without skipping
     `display:none`, so a closed card's text still counts towards its ancestor.
     `aria-describedby` has no such ambiguity: `HoverCard` writes the layer's
     own id onto its trigger, `DropdownMenuItem` sets no `aria-describedby` of
     its own, so this attribute is exactly one id, and an id matches exactly one
     element.
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
  await expect(page.getByRole('button', { name: 'Template: Small change' })).toBeVisible();

  const [createRequest] = await Promise.all([
    page.waitForRequest((pending) => pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/tracks'),
    page.getByRole('button', { name: 'Create track' }).click(),
  ]);
  const body = createRequest.postDataJSON() as Record<string, unknown>;
  expect(body).toMatchObject({ area_id: area.id, template_id: 'small-change' });
  expect(body).not.toHaveProperty('title');
  // Unbound template: the kernel rejects `template_input` against it.
  expect(body).not.toHaveProperty('template_input');

  await expect(page).toHaveURL(/\/track\/[0-9a-f-]+$/i);
  await expect(page.locator('[data-nc-page-title]', { hasText: 'Untitled track' })).toBeVisible();
  // The kernel accepted the template and stored the binding — the assertion
  // that separates "the picker put a field on the wire" from "the track was
  // actually created from that template".
  const trackId = /\/track\/([0-9a-f-]+)$/i.exec(page.url())?.[1];
  expect(trackId).toBeTruthy();
  const detail = await request.get(`/api/tracks/${trackId}`);
  expect(detail.ok()).toBe(true);
  const detailBody = await detail.json() as {
    track: { template_id: string | null };
    cards: { kind: string; payload: { body?: string } }[];
  };
  expect(detailBody.track.template_id).toBe('small-change');

  /* #1300 — the assertion this case's name always claimed and never made.
     `template_id` on the track row says the kernel accepted the binding; it says
     nothing about the report, which is the thing "seeds its report" is about.
     Before #1300 the report came from forking a hidden system-area track the
     kernel lazily seeded; it now comes from instantiating a Rust constant in
     the create transaction. Both produce the same document — that equivalence
     is pinned in-process by
     `track_template_tracks.rs::creating_from_a_template_instantiates_its_recipe`
     — and this is the live-stack half: through the real kernel, over HTTP, on
     the track the browser actually created.

     `ready: false` is asserted alongside the keys because it is the difference
     between "the plan is present" and "the plan is running". A template's tasks
     are pre-set, not released; an instantiation that shipped them ready would
     start dispatching work nobody approved. */
  const report = detailBody.cards.find((card) => card.kind === 'track-report');
  expect(report, 'the created track must have a track-report card').toBeTruthy();
  const reportBody = report?.payload.body ?? '';
  for (const key of ['inspect', 'implement', 'verify']) {
    expect(reportBody, `small-change must pre-set the ${key} task`).toContain(`"key": "${key}"`);
  }
  expect(reportBody).toContain('"ready": false');
  expect(reportBody).not.toContain('"ready": true');

  expect(errors).toEqual([]);
});
