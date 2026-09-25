import { expect, test, type Page, type Request } from '@playwright/test';
import { createArea } from './helpers/seed.js';

const createdAreaIds: string[] = [];

/* astryx puts `label` on the `contenteditable` as `aria-label`; the placeholder is the empty-state prompt beside it. */
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

/* Asserts the `title` KEY is absent from the POST, not a value: an empty string reaches the same
 * stored title but says this client decided the name. */
test('creates a track from an Area group with no title, and persists it', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  /* Every create this page emits: the create carries no idempotency key, so a second POST is a
       second track AND a second delivery of the same sentence. */
  const creates: Request[] = [];
  page.on('request', (pending) => {
    if (pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/tracks') {
      creates.push(pending);
    }
  });
  /* `harness.user_message.enqueued` is the one signal at this tier that the SERVER queued the message.
   * Registered before `goto`: the app subscribes to `['*']` with a replay cursor on first paint. */
  const frames: Record<string, unknown>[] = [];
  page.on('websocket', (socket) => {
    if (new URL(socket.url()).pathname !== '/api/events') return;
    socket.on('framereceived', (frame) => {
      if (typeof frame.payload !== 'string') return;
      try {
        const parsed: unknown = JSON.parse(frame.payload);
        if (typeof parsed === 'object' && parsed !== null) frames.push(parsed as Record<string, unknown>);
      } catch {
        /* Not JSON — a keepalive or a partial frame. */
      }
    });
  });
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  await page.goto('/next/');
  await page.getByRole('button', { name: `New track in ${area.name}` }).click();

  /* `waitForURL` is the surface being ready; the composer visible is it being usable. */
  await page.waitForURL(/\/area\/[^/]+\/new$/);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  const message = `FE e2e track ${Date.now()}`;
  await expect(page.getByLabel(TASK_LABEL)).toBeVisible();
  await expect(page.getByText(TASK_PLACEHOLDER)).toBeVisible();
  // The default path must stay byte-identical: the kernel keys its managed-workspace branch on the
  // absence of `cwd`, so a control defaulting to `$HOME` or `""` would silently attach every track.
  await expect(page.getByRole('button', { name: 'Folder: Neige workspace' })).toBeVisible();
  // The picker is collapsed, so "what is selected" is read off the trigger's accessible name.
  await expect(page.getByRole('button', { name: 'Template: No template' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Create track' })).toBeDisabled();
  await page.getByLabel(TASK_LABEL).fill(message);
  await expect(page.getByRole('button', { name: 'Create track' })).toBeEnabled();
  const [createRequest] = await Promise.all([
    page.waitForRequest((pending) => pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/tracks'),
    page.getByRole('button', { name: 'Create track' }).click(),
  ]);
  const body = createRequest.postDataJSON() as Record<string, unknown>;
  expect(body).toMatchObject({ area_id: area.id, planner_provider: 'codex' });
  expect(body).toHaveProperty('theme');
  expect(body).not.toHaveProperty('title');
  /* Asserted against the typed string, so a create that posted other text is not confused with delivery. */
  expect(body).toMatchObject({ first_message: message });
  /* 201, not "some 2xx": `first_message` is validated before anything is minted, so a rejected sentence is a 400. */
  expect((await createRequest.response())?.status(), 'the create carrying the sentence must be accepted').toBe(201);
  expect(body).not.toHaveProperty('cwd');
  expect(body).not.toHaveProperty('attach_folder');
  // No-template must not send these: the kernel 400s an empty `template_id` and the body is `deny_unknown_fields`.
  expect(body).not.toHaveProperty('template_id');
  expect(body).not.toHaveProperty('template_input');

  await expect(page).toHaveURL(/\/track\/[0-9a-f-]+$/i);
  /* Untitled is the normal landing state; the fallback is a placeholder, so the rename box opens blank. */
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
  const detail = await request.get(`/api/tracks/${trackId ?? ''}`);
  expect(detail.ok()).toBe(true);
  const cards = (await detail.json() as { cards: { id: string; kind: string; payload: unknown }[] }).cards;
  const plannerCard = cards.find((card) => card.kind === 'codex'
    && typeof card.payload === 'object' && card.payload !== null
    && (card.payload as { planner_harness?: unknown }).planner_harness === true);
  expect(plannerCard, 'the created track must carry a planner card').toBeTruthy();

  /* The kernel writes the drained sentence as a `userMessage` row (`turn_id: null`, `_projection`) before
   * `turn/start`; the fixture emits no items, so the row read back is the kernel's own. The drawer is
   * located by the control only it has: its name derives from its turns, which is the thing under test. */
  const drawer = page.locator('[role="complementary"]')
    .filter({ has: page.getByRole('button', { name: 'Close conversation' }) });
  await expect(drawer).toBeVisible();
  await expect(drawer.locator('[data-nc-turn="you"]')).toHaveText(message);
  await expect(drawer.locator('[data-nc-thread-empty]')).toHaveCount(0);
  const plannerItems = await request.get(`/api/cards/${plannerCard?.id ?? ''}/harness/items`);
  expect(plannerItems.ok()).toBe(true);
  const plannerRows = await plannerItems.json() as {
    item_type: string | null; method: string; turn_id: string | null; item_uuid: string | null;
    params: string; input_segments?: { presentation: string; text: string }[];
  }[];
  expect(
    plannerRows.map((row) => [row.item_type, row.method, row.turn_id]),
    'the kernel writes exactly one row for the drained sentence, before any echo (#1625 P2)',
  ).toEqual([['userMessage', 'item/completed', null]]);
  const projection = plannerRows[0];
  expect(projection?.input_segments?.map((segment) => segment.presentation)).toEqual(['user']);
  expect(projection?.input_segments?.[0]?.text).toContain(message);
  const projectionParams = JSON.parse(projection?.params ?? '{}') as { _projection?: unknown; item?: { clientId?: unknown } };
  expect(projectionParams._projection, 'the row is the kernel\'s own, not an echo').toBe(true);
  expect(projectionParams.item?.clientId, 'keyed by the id the drain sent codex').toBe(projection?.item_uuid);

  /* `waitForRequest` returned on the first create; give the page a bounded moment, then count again. */
  await page.waitForTimeout(1_000);
  expect(creates, 'the create carrying the sentence must happen exactly once').toHaveLength(1);
  expect((creates[0]?.postDataJSON() as { first_message?: unknown }).first_message).toBe(message);

  /* `char_count` is asserted because the event carries no message body. */
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

/* `small-change` is a template in every environment, bound to no plugin, so this does not depend on git-forge. */
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

  const option = page.getByRole('menuitem', { name: /^Small change/ });
  await expect(option).toBeVisible();
  await expect(page.getByText(/^\d+ tasks?$/)).toHaveCount(0);
  await option.hover();
  expect(await option.getAttribute('aria-describedby')).toBeNull();

  await option.click();
  await expect(page.getByRole('button', { name: 'Template: Small change' })).toBeVisible();

  const [createRequest] = await Promise.all([
    page.waitForRequest((pending) => pending.method() === 'POST' && new URL(pending.url()).pathname === '/api/tracks'),
    page.getByRole('button', { name: 'Create track' }).click(),
  ]);
  const body = createRequest.postDataJSON() as Record<string, unknown>;
  /* A template create carries the sentence too; the kernel runs the same harness start for both. */
  expect(body).toMatchObject({ area_id: area.id, planner_provider: 'codex', template_id: 'small-change', first_message: message });
  expect(body).not.toHaveProperty('title');
  // Unbound template: the kernel rejects `template_input` against it.
  expect(body).not.toHaveProperty('template_input');

  await expect(page).toHaveURL(/\/track\/[0-9a-f-]+$/i);
  await expect(page.locator('[data-nc-page-title]', { hasText: 'Untitled track' })).toBeVisible();
  const trackId = /\/track\/([0-9a-f-]+)$/i.exec(page.url())?.[1];
  expect(trackId).toBeTruthy();
  const detail = await request.get(`/api/tracks/${trackId}`);
  expect(detail.ok()).toBe(true);
  const detailBody = await detail.json() as {
    track: { template_id: string | null };
    cards: { kind: string; payload: { body?: string } }[];
  };
  expect(detailBody.track.template_id).toBe('small-change');

  const report = detailBody.cards.find((card) => card.kind === 'track-report');
  expect(report, 'the created track must have a report').toBeTruthy();
  const reportBody = report?.payload.body ?? '';
  expect(reportBody).not.toContain('```neige-block task');
  expect(reportBody).toContain('# 概要');
  expect(reportBody).toContain('# 已完成');
  await expect(page.locator('[data-nc-task-state]')).toHaveCount(0);


  expect(errors).toEqual([]);
});

test('creates the planner with the model and effort selected beside Send', async ({ page, request }) => {
  // A deterministic roster for the UI; creation and the persisted planner read still use the real kernel.
  await page.route((url) => url.pathname === '/api/models' && url.searchParams.get('provider') === 'codex', (route) => route.fulfill({ json: {
    models: [{ id: 'e2e-model', model: 'e2e-model', display_name: 'E2E model', description: '',
      is_default: false, default_reasoning_effort: 'low', supported_reasoning_efforts: [
        { reasoning_effort: 'low', description: 'Faster' },
        { reasoning_effort: 'high', description: 'More reasoning' },
      ] }],
    default: { model: null, reasoning_effort: null }, default_source: 'unknown',
    source: 'live', fetched_at_ms: 1,
  } }));
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  await page.goto('/next/');
  await page.getByRole('button', { name: `New track in ${area.name}` }).click();
  await page.getByRole('button', { name: 'Model: Default' }).click();
  await page.getByRole('menuitem', { name: 'E2E model' }).click();
  await page.getByRole('button', { name: 'Reasoning effort: low (the default)' }).click();
  await page.getByRole('menuitem', { name: /high/ }).click();
  await page.getByLabel(TASK_LABEL).fill('Start with the selected configuration');
  const creation = page.waitForResponse((response) => response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/tracks');
  await page.getByRole('button', { name: 'Create track' }).click();
  const response = await creation;
  expect(response.status()).toBe(201);
  expect(response.request().postDataJSON()).toMatchObject({ model: 'e2e-model', reasoning_effort: 'high',
    first_message: 'Start with the selected configuration' });
  const track = await response.json() as { id: string };
  await expect(page).toHaveURL(new RegExp(`/track/${track.id}$`));
  const detailResponse = await request.get(`/api/tracks/${track.id}`);
  expect(detailResponse.ok()).toBe(true);
  const detail = await detailResponse.json() as { cards: { id: string; payload: { planner_harness?: boolean } }[] };
  const planner = detail.cards.find((card) => card.payload.planner_harness === true);
  if (planner === undefined) throw new Error('Created track has no planner card');
  const run = await request.get(`/api/cards/${planner.id}/planner/run`);
  expect(run.ok()).toBe(true);
  expect(await run.json()).toMatchObject({ model: 'e2e-model', reasoning_effort: 'high' });
});

/** A deterministic Codex roster for the picker; creation still goes to the real kernel. */
async function routeCodexCatalog(page: Page) {
  await page.route((url) => url.pathname === '/api/models' && url.searchParams.get('provider') === 'codex', (route) => route.fulfill({ json: {
    models: [{ id: 'e2e-model', model: 'e2e-model', display_name: 'E2E model', description: '',
      is_default: false, default_reasoning_effort: 'low', supported_reasoning_efforts: [] }],
    default: { model: null, reasoning_effort: null }, default_source: 'unknown',
    source: 'live', fetched_at_ms: 1,
  } }));
}

/* The e2e stack runs without `--claude-planner-config`, so the kernel answers the Claude catalog
 * `unavailable` and the picker offers Codex alone (#1810). */
test('offers no Claude group on a kernel without Claude Planners', async ({ page, request }) => {
  await routeCodexCatalog(page);
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const claudeCatalog = page.waitForResponse((response) => new URL(response.url()).pathname === '/api/models'
    && new URL(response.url()).searchParams.get('provider') === 'claude');
  await page.goto('/next/');
  await page.getByRole('button', { name: `New track in ${area.name}` }).click();
  expect(await (await claudeCatalog).json()).toMatchObject({ source: 'unavailable', models: [] });
  await page.getByRole('button', { name: 'Model: Default' }).click();
  await expect(page.getByRole('menuitem', { name: 'E2E model' })).toBeVisible();
  await expect(page.getByRole('group', { name: 'Claude' })).toHaveCount(0);
});

/* With the Claude catalog served as a kernel with the flag would, a Claude pick reaches this kernel,
 * which refuses it (it runs without the flag); the refusal is shown, not hidden, and the same sentence
 * then creates on Codex. The real `claude` binary is never needed. */
test('a Claude pick carries provider and model to the kernel, and a Codex pick creates instead', async ({ page, request }) => {
  /* The one expected console error is the browser logging the refused create itself; anything else fails. */
  const errors: string[] = [];
  page.on('console', (entry) => {
    if (entry.type() !== 'error') return;
    const url = entry.location().url;
    const refusedCreate = entry.text().includes('status of 400') && url !== ''
      && new URL(url).pathname === '/api/tracks';
    if (!refusedCreate) errors.push(entry.text());
  });
  page.on('pageerror', (error) => errors.push(error.message));
  await routeCodexCatalog(page);
  await page.route((url) => url.pathname === '/api/models' && url.searchParams.get('provider') === 'claude', (route) => route.fulfill({ json: {
    models: [['opus', 'Opus'], ['sonnet', 'Sonnet'], ['haiku', 'Haiku']].map(([alias, name]) => ({
      id: alias, model: alias, display_name: name, description: '', is_default: false,
      supported_reasoning_efforts: [], default_reasoning_effort: null,
    })),
    default: { model: null, reasoning_effort: null }, default_source: 'unknown',
    source: 'built_in', fetched_at_ms: null,
  } }));
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  await page.goto('/next/');
  await page.getByRole('button', { name: `New track in ${area.name}` }).click();
  await page.waitForURL(/\/area\/[^/]+\/new$/);
  const message = `FE e2e Claude planner ${Date.now()}`;
  await page.getByLabel(TASK_LABEL).fill(message);
  await page.getByRole('button', { name: 'Model: Codex Default' }).click();
  await page.getByRole('group', { name: 'Claude' }).getByRole('menuitem', { name: 'Sonnet' }).click();
  await expect(page.getByRole('button', { name: 'Model: Claude Sonnet' })).toBeVisible();
  await expect(page.getByRole('button', { name: /^Reasoning effort:/ })).toHaveCount(0);

  const refused = page.waitForResponse((response) => response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/tracks');
  await page.getByRole('button', { name: 'Create track' }).click();
  const refusal = await refused;
  expect(refusal.status()).toBe(400);
  const refusedBody = refusal.request().postDataJSON() as Record<string, unknown>;
  expect(refusedBody).toMatchObject({ area_id: area.id, planner_provider: 'claude', model: 'sonnet', first_message: message });
  expect(refusedBody).not.toHaveProperty('reasoning_effort');
  await expect(page.getByRole('alert')).toContainText('--claude-planner-config');
  await expect(page).toHaveURL(/\/area\/[^/]+\/new$/);

  /* Reopened by keyboard: astryx swallows a trigger click that lands right after its menu hid. */
  await page.getByRole('button', { name: 'Model: Claude Sonnet' }).focus();
  await page.keyboard.press('ArrowDown');
  await page.getByRole('group', { name: 'Codex' }).getByRole('menuitem', { name: /^Default/ }).click();
  const created = page.waitForResponse((response) => response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/tracks');
  await page.getByRole('button', { name: 'Create track' }).click();
  const creation = await created;
  expect(creation.status()).toBe(201);
  const createdBody = creation.request().postDataJSON() as Record<string, unknown>;
  expect(createdBody).toMatchObject({ planner_provider: 'codex', first_message: message });
  expect(createdBody).not.toHaveProperty('model');
  await expect(page).toHaveURL(/\/track\/[0-9a-f-]+$/i);
  expect(errors).toEqual([]);
});
