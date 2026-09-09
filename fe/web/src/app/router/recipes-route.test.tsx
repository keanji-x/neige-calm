// @vitest-environment jsdom
//
// `/recipes` and the recipe half of `/area/{id}/new` (#1292 S4), driven through
// the real router, the real QueryClient and a fake transport — so every
// assertion below is about the bytes that would go on the wire, not about a
// fixture agreeing with itself.
//
// **The body editor is mocked, and only the body editor.** CodeMirror measures
// a layout jsdom does not have, which is why `systems/fs-viewers` mocks its
// pane in the same tier for the same reason. What is under test here is not
// the text widget: it is what a save sends, what the screen renders once the
// server answers, and what a conflict leaves standing. Those are the three
// things a wrong implementation gets wrong, and none of them needs a real
// editor to be wrong. The widget itself is exercised for real in
// `features/report/recipe/recipe.browser.test.tsx`.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('../../features/report/recipe/body-editor.tsx', () => ({
  RecipeBodyEditor: ({ value, label, onChange }: {
    value: string; label: string; onChange: (next: string) => void;
  }) => (
    <textarea aria-label={label} value={value} onChange={(event) => onChange(event.target.value)} />
  ),
}));

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { queryKeys } from '../providers/queries.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
  };
}

const AREA = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };

const TRACK_ROW = {
  id: 'w-new', area_id: 'c1', title: '', sort: 0, archived_at: null, pinned_at: null,
  lifecycle: 'draft', cwd: '/srv/managed', template_id: null, plugin_scope: null,
  purpose: null, template_input: null, terminal_at: null, created_at: 1, updated_at: 1,
};

/* The body as the server holds it: already canonical, because the write
   boundary canonicalised it on the way in. */
const STORED_BODY = '## Ship checklist\n\nWrite the change.\n';

const RECIPE = {
  id: 'r-ship', title: 'Ship checklist', body: STORED_BODY,
  revision: 7, created_at: 1, updated_at: 2,
};

const BODY_FIELD = 'Recipe body, Markdown';

type Options = Readonly<{
  recipes?: unknown;
  /**
   * `GET /api/track-recipes`, answered per call — the 0-based read index — so a
   * test can say what the list held *before* a write and what it holds once the
   * refetch that write queued lands. `recipes` is the constant-answer form of
   * the same thing; a test gives one or the other.
   */
  recipeList?: (call: number) => ApiTransportResponse | Promise<ApiTransportResponse>;
  templates?: unknown;
  /** What `PUT /api/track-recipes/{id}` answers. */
  put?: ApiTransportResponse;
  /** What `POST /api/track-recipes` answers — the create. */
  post?: ApiTransportResponse;
  /** What `DELETE /api/track-recipes/{id}` answers. */
  remove?: ApiTransportResponse;
  /** Server-compiled snapshot; structured preview tests supply it explicitly. */
  preview?: ApiTransportResponse | ((request: ApiRequest) => ApiTransportResponse | Promise<ApiTransportResponse>);
}>;

function harness(options: Options = {}) {
  const sent: ApiRequest[] = [];
  let listReads = 0;
  const saved = new Map<string, { id: string; title: string; body: string; revision: number }>();
  const remember = (value: unknown) => {
    if (value !== null && typeof value === 'object' && 'id' in value && typeof value.id === 'string'
      && 'title' in value && typeof value.title === 'string' && 'body' in value && typeof value.body === 'string'
      && 'revision' in value && typeof value.revision === 'number') {
      const previous = saved.get(value.id);
      if (previous === undefined || previous.revision <= value.revision) saved.set(value.id, { id: value.id, title: value.title, body: value.body, revision: value.revision });
    }
  };
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      if (request.method === 'GET' && request.path.includes('/preview?')) {
        if (options.preview !== undefined) return Promise.resolve(typeof options.preview === 'function' ? options.preview(request) : options.preview);
        const url = new URL(request.path, 'http://fixture.local');
        const recipe = saved.get(decodeURIComponent(url.pathname.split('/')[3]));
        if (recipe === undefined) return Promise.resolve({ status: 404, statusText: 'Not Found', body: { error: 'Recipe gone' } });
        if (Number(url.searchParams.get('if_revision')) !== recipe.revision) return Promise.resolve({ status: 409, statusText: 'Conflict', body: { error: 'Recipe changed' } });
        // Existing cases use prose-only bodies. Structured cases provide an
        // explicit compiled response; this is not a fence parser.
        return Promise.resolve({ status: 200, statusText: 'OK', body: { id: recipe.id, revision: recipe.revision,
          payload: { summary: recipe.title, body: recipe.body, blocks: [{ id: `body-${recipe.revision}`, kind: 'prose', payload: { markdown: recipe.body } }] },
        } });
      }
      if (request.method === 'PUT' && request.path.startsWith('/api/track-recipes/')) {
        const response = options.put ?? { status: 200, statusText: 'OK', body: RECIPE };
        if (response.status === 200) remember(response.body);
        return Promise.resolve(response);
      }
      if (request.method === 'DELETE' && request.path.startsWith('/api/track-recipes/')) {
        return Promise.resolve(options.remove ?? { status: 204, statusText: 'No Content', body: null });
      }
      if (request.method === 'POST' && request.path === '/api/track-recipes') {
        const response = options.post ?? { status: 200, statusText: 'OK', body: RECIPE };
        if (response.status === 200 || response.status === 201) remember(response.body);
        return Promise.resolve(response);
      }
      if (request.path === '/api/track-recipes') {
        const call = listReads;
        listReads += 1;
        return Promise.resolve(options.recipeList?.(call) ?? { status: 200, statusText: 'OK', body: options.recipes ?? [RECIPE] })
          .then(response => {
            if (response.status === 200 && Array.isArray(response.body)) response.body.forEach(remember);
            return response;
          });
      }
      if (request.path === '/api/track-templates') {
        return Promise.resolve({ status: 200, statusText: 'OK', body: options.templates ?? [] });
      }
      if (request.method === 'POST' && request.path === '/api/tracks') {
        return Promise.resolve({ status: 200, statusText: 'OK', body: TRACK_ROW });
      }
      if (request.method === 'GET' && request.path === '/api/tracks/w-new') {
        return Promise.resolve({
          status: 200,
          statusText: 'OK',
          body: { track: { ...TRACK_ROW }, can_resume: false, cards: [], overlays: [] },
        });
      }
      const body = request.path === '/api/areas' ? [AREA] : [];
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
  });
  render(
    <StrictMode>
      <QueryClientProvider client={client}>
        <ThemeProvider storage={memoryStorage()}>
          <RouterProvider router={router} />
        </ThemeProvider>
      </QueryClientProvider>
    </StrictMode>,
  );
  return { sent, client, listReads: () => listReads };
}

function atRecipes(options: Options = {}) {
  window.history.pushState({}, '', `${APP_BASEPATH}/recipes`);
  return harness(options);
}

function atNewTrack(options: Options = {}) {
  window.history.pushState({}, '', `${APP_BASEPATH}/area/c1/new`);
  return harness(options);
}

/** Open the one recipe in the list and put it into edit mode. */
async function openForEditing(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: 'Ship checklist' }));
  await user.click(await screen.findByRole('button', { name: 'Edit' }));
  return screen.getByRole('textbox', { name: BODY_FIELD });
}

function lastPut(sent: readonly ApiRequest[]): ApiRequest | undefined {
  return [...sent].reverse().find((request) => request.method === 'PUT');
}

/* The recipe as the create resolved to it: a body the client never sent,
   because the write boundary re-rendered every fence on the way in. Every
   assertion about the create path is about *this* text reaching the screen. */
const CREATED = {
  id: 'r-new', title: 'Ship checklist', body: 'Canonicalised by the server.\n',
  revision: 1, created_at: 3, updated_at: 3,
};

const OK = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

function prosePreview(recipe: typeof RECIPE) {
  return OK({ id: recipe.id, revision: recipe.revision, payload: { summary: recipe.title, body: recipe.body,
    blocks: [{ id: 'saved-prose', kind: 'prose', payload: { markdown: recipe.body } }],
  } });
}

it('advances a preview conflict to the latest saved revision without reopening the editor', async () => {
  const user = userEvent.setup();
  const latest = { ...RECIPE, revision: 9, title: 'New saved title', body: 'Revision nine content.' };
  const { sent } = atRecipes({ recipeList: call => OK(call === 0 ? [RECIPE] : [latest]),
    preview: request => request.path.endsWith('=9') ? prosePreview(latest)
      : { status: 409, statusText: 'Conflict', body: { error: 'Recipe changed' } },
  });
  await user.click(await screen.findByRole('button', { name: 'Ship checklist' }));
  expect(await screen.findByRole('heading', { name: 'New saved title', level: 1 })).toBeTruthy();
  expect(await screen.findByText('Revision nine content.')).toBeTruthy();
  expect(sent.some(request => request.path.endsWith('/preview?if_revision=9'))).toBe(true);
  expect(screen.queryByText(/请重试读取最新版本/)).toBeNull();
});

it('keeps the editing baseline when conflict refresh finishes after Edit was entered', async () => {
  const user = userEvent.setup();
  const latest = { ...RECIPE, revision: 9, title: 'New saved title', body: 'Revision nine content.' };
  let finishList!: (response: ApiTransportResponse) => void;
  const refreshed = new Promise<ApiTransportResponse>(resolve => { finishList = resolve; });
  const { sent, client, listReads } = atRecipes({ recipeList: call => call === 0 ? OK([RECIPE]) : refreshed,
    preview: request => request.path.endsWith('=9') ? prosePreview(latest)
      : { status: 409, statusText: 'Conflict', body: { error: 'Recipe changed' } },
    put: { status: 409, statusText: 'Conflict', body: { error: 'stale if_revision' } },
  });
  await user.click(await screen.findByRole('button', { name: 'Ship checklist' }));
  await waitFor(() => expect(listReads()).toBeGreaterThan(1));
  await user.click(screen.getByRole('button', { name: 'Edit' }));
  const field = screen.getByRole('textbox', { name: BODY_FIELD });
  await user.clear(field);
  await user.type(field, 'Keep my unfinished draft.');
  await act(async () => { finishList(OK([latest])); await refreshed; });
  await waitFor(() => expect(client.getQueryData(queryKeys.trackRecipes())).toEqual([latest]));
  expect(field).toHaveProperty('value', 'Keep my unfinished draft.');
  await user.click(screen.getByRole('button', { name: 'Save' }));
  await waitFor(() => expect(lastPut(sent)?.body).toMatchObject({ if_revision: 7, body: 'Keep my unfinished draft.' }));
  expect(field).toHaveProperty('value', 'Keep my unfinished draft.');
  await user.click(screen.getByRole('button', { name: 'Cancel' }));
  expect(await screen.findByText('Revision nine content.')).toBeTruthy();
});

it('aborts a pending preview retry and preserves draft text and revision on Edit', async () => {
  const user = userEvent.setup();
  let retry = false;
  const pending: { request: ApiRequest; resolve: (response: ApiTransportResponse) => void }[] = [];
  const { sent } = atRecipes({ preview: request => retry
    ? new Promise<ApiTransportResponse>(resolve => { pending.push({ request, resolve }); })
    : { status: 503, statusText: 'Unavailable', body: { error: 'Preview temporarily unavailable' } },
    put: { status: 409, statusText: 'Conflict', body: { error: 'stale if_revision' } },
  });
  await user.click(await screen.findByRole('button', { name: 'Ship checklist' }));
  expect(await screen.findByText('Preview temporarily unavailable')).toBeTruthy();
  retry = true;
  await user.click(screen.getByRole('button', { name: '重试预览' }));
  await waitFor(() => expect(pending.length).toBeGreaterThan(0));
  await user.click(screen.getByRole('button', { name: 'Edit' }));
  const field = screen.getByRole('textbox', { name: BODY_FIELD });
  await user.clear(field);
  await user.type(field, 'Draft while preview was loading.');
  expect(pending.every(item => item.request.signal?.aborted)).toBe(true);
  await act(async () => { pending.forEach(item => item.resolve(prosePreview(RECIPE))); await Promise.resolve(); });
  expect(field).toHaveProperty('value', 'Draft while preview was loading.');
  await user.click(screen.getByRole('button', { name: 'Save' }));
  await waitFor(() => expect(lastPut(sent)?.body).toMatchObject({ if_revision: 7, body: 'Draft while preview was loading.' }));
});

it('renders a saved recipe through its server-compiled native Report preview', async () => {
  const user = userEvent.setup();
  const layout = { version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [{
    kind: 'table', title: 'Saved table', span: 1, data: { rows: [] },
    columns: [{ key: 'asset', label: 'Saved asset column', format: 'text', digits: 0 }],
  }] };
  const recipe = { ...RECIPE, body: `\`\`\`neige-block layout\n${JSON.stringify(layout)}\n\`\`\`\n` };
  const { sent } = atRecipes({ recipes: [recipe], preview: OK({
    id: recipe.id, revision: recipe.revision,
    payload: { summary: recipe.title, body: recipe.body, blocks: [{ id: 'compiled-layout', rev: 1, kind: 'layout', payload: layout }] },
  }) });
  await user.click(await screen.findByRole('button', { name: 'Ship checklist' }));
  expect(await screen.findByRole('columnheader', { name: 'Saved asset column' })).toBeTruthy();
  expect(document.querySelector('[data-nc-recipe-rendered] pre')).toBeNull();
  expect(sent.some(request => request.path === '/api/track-recipes/r-ship/preview?if_revision=7')).toBe(true);
});

/** Compose a recipe and save it. Leaves the screen wherever the save put it. */
async function composeAndSave(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: 'New recipe' }));
  const field = await screen.findByRole('textbox', { name: BODY_FIELD });
  await user.type(field, 'What the author typed.');
  await user.click(screen.getByRole('button', { name: 'Save' }));
}

describe('the recipe editor', () => {
  /*
   * The load half of the round trip. An editor that normalised, re-indented or
   * re-wrapped what it loaded would send back something the author never
   * touched — and because the write boundary accepts it, the damage would be
   * stored silently. Saving an untouched recipe must therefore be a byte-exact
   * echo, `if_revision` included.
   */
  it('sends back exactly what it loaded when nothing was edited', async () => {
    const user = userEvent.setup();
    const { sent } = atRecipes();
    await openForEditing(user);
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(lastPut(sent)).toBeDefined());
    const put = lastPut(sent);
    expect(put?.path).toBe('/api/track-recipes/r-ship');
    expect(put?.body).toEqual({ title: 'Ship checklist', body: STORED_BODY, if_revision: 7 });
  });

  /*
   * The single most likely wrong implementation of this screen: render the
   * draft after saving it. It looks right in every case where the server
   * changes nothing — which is most of them — and hides the one thing the
   * author most needs to see, because the write boundary *does* rewrite bodies
   * (fences re-rendered, tombstones dropped, privilege fields normalized).
   *
   * So the stub answers with a body the client never sent, and both halves are
   * asserted: the server's text is on screen and the sent text is not.
   */
  it('renders the server response after a save, not the local draft', async () => {
    const user = userEvent.setup();
    const rewritten = { ...RECIPE, body: 'Canonicalised by the server.\n', revision: 8 };
    const { sent } = atRecipes({ put: { status: 200, statusText: 'OK', body: rewritten } });
    const field = await openForEditing(user);
    await user.clear(field);
    await user.type(field, 'What the author typed.');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    expect(await screen.findByText('Canonicalised by the server.')).toBeTruthy();
    expect(screen.queryByText('What the author typed.')).toBeNull();
    // …and the draft is what was *sent*, so this is a claim about rendering
    // rather than about the request.
    expect((lastPut(sent)?.body as { body: string }).body).toBe('What the author typed.');
  });

  /*
   * A conflict costs a re-read. It must not also cost the edit: the author's
   * text is the only thing in this exchange that exists nowhere else.
   */
  it('keeps the draft when the recipe changed underneath the writer', async () => {
    const user = userEvent.setup();
    atRecipes({
      put: { status: 409, statusText: 'Conflict', body: { error: 'stale if_revision' } },
    });
    const field = await openForEditing(user);
    await user.clear(field);
    await user.type(field, 'Half-finished thought.');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(screen.getByText(/changed somewhere else/)).toBeTruthy());
    expect(screen.getByRole('textbox', { name: BODY_FIELD })).toHaveProperty(
      'value', 'Half-finished thought.',
    );
    // Still editing — a conflict does not drop the reader back into the
    // rendered view, where the draft would have nowhere to live.
    expect(screen.getByRole('button', { name: 'Save' })).toBeTruthy();
  });

  /*
   * The other half of that conflict: the notice tells the reader to close and
   * reopen the recipe to start from the current version, and this asserts that
   * doing so actually gets them one. It is a claim about the cache, not about
   * the wording — reopening re-seeds the editor from the list's row, so if the
   * 409 left the list holding the revision the server has already moved past,
   * the reader reopens onto the same stale text and the next Save conflicts
   * again, forever.
   */
  it('yields the current version when the reader takes the conflict notice up on it', async () => {
    const user = userEvent.setup();
    const moved = { ...RECIPE, body: "The other window's version.\n", revision: 9 };
    atRecipes({
      recipeList: (call) => OK(call === 0 ? [RECIPE] : [moved]),
      put: { status: 409, statusText: 'Conflict', body: { error: 'stale if_revision' } },
    });
    const field = await openForEditing(user);
    await user.clear(field);
    await user.type(field, 'Half-finished thought.');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(screen.getByText(/changed somewhere else/)).toBeTruthy());

    // Close and reopen, exactly as the notice says to.
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    await user.click(screen.getByRole('button', { name: 'All recipes' }));
    await user.click(await screen.findByRole('button', { name: 'Ship checklist' }));

    expect(await screen.findByText("The other window's version.")).toBeTruthy();
  });

  /*
   * `if_revision` is the author's read, and nothing else's.
   *
   * The editor seeds from its prop once and then holds the row in state, and
   * this is the assertion that says why: the list underneath it refetches on
   * its own schedule, and if the gate read that prop instead of the state, a
   * refetch landing mid-edit would silently re-point the write at a revision
   * the author never saw — turning the conflict the `if_revision` gate exists
   * to raise into a clean overwrite of somebody else's work.
   */
  it('gates the save on the revision the author opened, not on one a refetch brought in', async () => {
    const user = userEvent.setup();
    const moved = { ...RECIPE, body: 'Moved under the editor.\n', revision: 99 };
    const { sent, client } = atRecipes({ recipeList: (call) => OK(call === 0 ? [RECIPE] : [moved]) });
    const field = await openForEditing(user);
    await user.clear(field);
    await user.type(field, 'Still the version I opened.');

    // A background refetch, landed and observed in the cache before the save.
    await act(async () => { await client.refetchQueries({ queryKey: queryKeys.trackRecipes() }); });
    await waitFor(() => expect(
      client.getQueryData<{ revision: number }[]>(queryKeys.trackRecipes())?.[0]?.revision,
    ).toBe(99));

    await user.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(lastPut(sent)).toBeDefined());
    expect(lastPut(sent)?.body).toEqual({
      title: 'Ship checklist', body: 'Still the version I opened.', if_revision: 7,
    });
  });

  /*
   * A delete that the server refused. The dialog used to close on the promise
   * regardless and drop the rejection on the floor: no banner, no state change,
   * and a list still showing the recipe with nothing on screen saying why.
   */
  it('says so when the delete fails, instead of closing as if it had worked', async () => {
    const user = userEvent.setup();
    atRecipes({
      remove: { status: 500, statusText: 'Internal Server Error', body: { error: 'Storage is offline.' } },
    });
    await user.click(await screen.findByRole('button', { name: 'Ship checklist' }));
    await user.click(await screen.findByRole('button', { name: 'Delete' }));
    await user.click(await screen.findByRole('button', { name: 'Delete recipe' }));

    expect(await screen.findByText('Storage is offline.')).toBeTruthy();
    // Still on the recipe: nothing was destroyed, so nothing was left behind.
    expect(screen.getByRole('button', { name: 'Delete' })).toBeTruthy();
  });
});

describe('creating a recipe', () => {
  /*
   * The create path's version of the rule the whole editor is arranged around,
   * and the one place it is hardest to hold: the save resolves, the page moves
   * `open` to the brand-new id, and the list it would look that id up in is
   * still the list from before the create — the mutation's invalidate only
   * *queues* a refetch.
   *
   * So this stub's list read never returns the new row at all. Anything that
   * depends on the refetch catching up fails here; only a page that renders the
   * row the create resolved to passes. That is the difference between the rule
   * holding by construction and holding by timing.
   */
  it('renders the row the create resolved to, with no list refetch to lean on', async () => {
    const user = userEvent.setup();
    atRecipes({ recipeList: () => OK([]), post: OK(CREATED) });
    await composeAndSave(user);

    expect(await screen.findByText('Canonicalised by the server.')).toBeTruthy();
    expect(screen.queryByText('What the author typed.')).toBeNull();
    // Not dropped back to the list — which, with this stub, is empty.
    expect(screen.queryByText(/You have no recipes yet/)).toBeNull();
  });

  /*
   * And when that refetch does not merely lag but fails. `trackRecipesQueryOptions`
   * sets `retry: false`, so the cache keeps the previous list — the one without
   * the new row — and a page that trusted the list would strand the reader on a
   * list that does not show the recipe they just made, with no error naming the
   * one that actually happened.
   */
  it('keeps the reader on the new recipe when the list refetch fails', async () => {
    const user = userEvent.setup();
    const failed = { status: 500, statusText: 'Internal Server Error', body: { error: 'Storage is offline.' } };
    const { listReads } = atRecipes({
      recipeList: (call) => (call === 0 ? OK([]) : failed),
      post: OK(CREATED),
    });
    await composeAndSave(user);

    // The create's invalidate has produced a second read, and it failed.
    await waitFor(() => expect(listReads()).toBeGreaterThan(1));
    expect(await screen.findByText('Canonicalised by the server.')).toBeTruthy();
    expect(screen.queryByText(/You have no recipes yet/)).toBeNull();
  });
});

describe('creating a track from a recipe', () => {
  /*
   * `template_id` and `recipe_id` are mutually exclusive on the wire and the
   * kernel answers a request naming both with a 400. The picker's tagged union
   * makes that structural, and this is the assertion that the structure
   * reaches the request: exactly one key, and it is the right one.
   */
  it('sends recipe_id and never template_id', async () => {
    const user = userEvent.setup();
    const { sent } = atNewTrack({
      templates: [{ id: 'small-change', title: 'Small change', tasks: [] }],
    });
    const composer = await screen.findByRole('textbox', { name: 'What this track should do' });
    await user.click(composer);
    await user.keyboard('Ship it');
    await user.click(await screen.findByRole('button', { name: /^Template: / }));
    await user.click(await screen.findByRole('menuitem', { name: 'Ship checklist' }));
    await user.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(
      sent.some((request) => request.method === 'POST' && request.path === '/api/tracks'),
    ).toBe(true));
    const posted = sent.find((request) => request.method === 'POST' && request.path === '/api/tracks');
    const body = posted?.body as Record<string, unknown>;
    expect(body.recipe_id).toBe('r-ship');
    expect('template_id' in body).toBe(false);
    expect('template_input' in body).toBe(false);
  });
});
