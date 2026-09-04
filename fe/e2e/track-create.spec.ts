import { expect, test, type Page, type Request } from '@playwright/test';
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
 * became (S3). It travels as `first_message` on this same create (#1299), which
 * is asserted on the request and counted at the end of this case.
 */
test('creates a track from an Area group with no title, and persists it', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  /* Every create this page emits, not just the one `waitForRequest` returns.
     The sentence rides on the create and the create carries no idempotency key
     (#1384), so a second POST is a second track *and* a second delivery of the
     same sentence — countable here and nowhere else in this file. */
  const creates: Request[] = [];
  page.on('request', (pending) => {
    if (pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/tracks') {
      creates.push(pending);
    }
  });
  /*
   * The kernel's own account of the delivery, over the socket the app is
   * already on (#1299).
   *
   * `harness.user_message.enqueued` is emitted where the message is queued onto
   * the harness — not by this page, and not by the HTTP response — so it is the
   * one signal at this tier that says the *server* did the thing rather than
   * that the browser asked for it. It carries `track_id` and `char_count` and
   * no text, which is exactly enough: the track it names is the one just
   * created, and the count is the length of what was typed.
   *
   * It does **not** replace the in-process assertion in
   * `crates/calm-server/tests/cases/track_create_first_message.rs`: enqueued is
   * not "reached the agent's turn input", and only that suite can see the turn.
   * What this adds is that the browser's create really produced the kernel-side
   * enqueue, once, for this track.
   *
   * Registered before `goto` because the app opens the socket on first paint,
   * and it subscribes to `['*']` with a replay cursor, so an event emitted
   * during the create arrives here even though the page did not know the track
   * id when it subscribed.
   */
  const frames: Record<string, unknown>[] = [];
  page.on('websocket', (socket) => {
    if (new URL(socket.url()).pathname !== '/api/events') return;
    socket.on('framereceived', (frame) => {
      if (typeof frame.payload !== 'string') return;
      try {
        const parsed: unknown = JSON.parse(frame.payload);
        if (typeof parsed === 'object' && parsed !== null) frames.push(parsed as Record<string, unknown>);
      } catch {
        /* Not JSON — a keepalive or a partial frame. The assertion below counts
           what did parse, so junk cannot make it pass. */
      }
    });
  });
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  await page.goto('/next/');
  await page.getByRole('button', { name: `New track in ${area.name}` }).click();

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
  /* #1299 — and it is on this create, under the key the kernel seeds into the
     `planner-harness-start` transaction. Asserted against the *typed* string,
     so a create that posted some other text (or a trimmed-to-nothing one) is
     not confused with delivery. */
  expect(body).toMatchObject({ first_message: message });
  /* The real kernel accepted it. 201 and not merely "some 2xx": `first_message`
     is validated before anything is minted, so a rejected sentence is a 400
     here — this is the assertion that separates "the browser sent the key" from
     "the server took it". */
  expect((await createRequest.response())?.status(), 'the create carrying the sentence must be accepted').toBe(201);
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
   * #1299 — the agent the sentence was delivered to exists on the track.
   *
   * This is as far as *this* tier can go, and the limit is worth writing down
   * rather than papering over. The sentence lands in the harness as a queued
   * `Observation::UserMessage`; nothing over HTTP exposes that queue, and
   * `harness/items` only fills once the app-server echoes the turn's
   * `userMessage` back as an `item/completed`. CI runs this stack against the
   * `osc-probe-child` fixture, which answers `initialize` / `thread/start` /
   * `turn/start` and emits no items at all (see `e2e/README.md`) — so an
   * assertion that the text appears in `harness/items` would fail in CI for a
   * working delivery, and the `not.toContain` this replaces passed against an
   * empty list no matter what the kernel did. Both directions are vacuous.
   *
   * What proves the delivery reaches the agent exactly once is in-process,
   * against a harness this tier cannot reach:
   * `crates/calm-server/tests/cases/track_create_first_message.rs`
   * (`the_first_message_reaches_the_agent_exactly_once`). What this case owns is
   * the browser half — the sentence is on the create, the kernel took it, and
   * the create happened once.
   */
  const detail = await request.get(`/api/tracks/${trackId ?? ''}`);
  expect(detail.ok()).toBe(true);
  const cards = (await detail.json() as { cards: { id: string; kind: string; payload: unknown }[] }).cards;
  const plannerCard = cards.find((card) => card.kind === 'codex'
    && typeof card.payload === 'object' && card.payload !== null
    && (card.payload as { planner_harness?: unknown }).planner_harness === true);
  expect(plannerCard, 'the created track must carry a planner card').toBeTruthy();

  /*
   * Once, and still once after the interaction settles.
   *
   * `waitForRequest` returned on the *first* create, so the count taken at that
   * moment cannot see a second one emitted a tick later — which is exactly the
   * shape a retry-on-settle bug has, and with the sentence on the body it
   * double-delivers rather than merely double-creating. So: give the page a
   * bounded moment to finish misbehaving, then count. Same two-step as
   * `track-conversation-create.spec.ts`.
   */
  await page.waitForTimeout(1_000);
  expect(creates, 'the create carrying the sentence must happen exactly once').toHaveLength(1);
  expect((creates[0]?.postDataJSON() as { first_message?: unknown }).first_message).toBe(message);

  /*
   * And the kernel enqueued it onto the harness — exactly once, for this track.
   *
   * Counted after the same settle window as the creates above, and for the same
   * reason: a second create would show up here as a second enqueue. `char_count`
   * is asserted too, because a delivery of some *other* text would otherwise be
   * indistinguishable from this one (the event carries no message body).
   */
  const enqueued = frames.filter((frame) => frame.ev === 'harness.user_message.enqueued'
    && (frame.data as { track_id?: unknown } | undefined)?.track_id === trackId);
  expect(
    enqueued,
    'the kernel must enqueue the sentence onto the new track\'s harness exactly once',
  ).toHaveLength(1);
  expect((enqueued[0]?.data as { char_count?: unknown }).char_count).toBe([...message].length);

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

  await page.goto('/next/');
  await page.getByRole('button', { name: `New track in ${area.name}` }).click();
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
  /* #1299 — a template create carries the sentence too. The kernel runs the
     same harness start for both, so a create path that dropped the key only
     when a template was chosen would leave the case above green. */
  expect(body).toMatchObject({ area_id: area.id, template_id: 'small-change', first_message: message });
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
