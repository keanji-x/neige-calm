// @vitest-environment jsdom
// The body editor is mocked, and only it: CodeMirror measures a layout jsdom does not have.
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
  /** `GET /api/track-recipes`, answered per 0-based read index, so a test can say what the list held before a write and after its refetch. */
  recipeList?: (call: number) => ApiTransportResponse;
  templates?: unknown;
  /** What `PUT /api/track-recipes/{id}` answers. */
  put?: ApiTransportResponse;
  /** What `POST /api/track-recipes` answers — the create. */
  post?: ApiTransportResponse;
  /** What `DELETE /api/track-recipes/{id}` answers. */
  remove?: ApiTransportResponse;
}>;

function harness(options: Options = {}) {
  const sent: ApiRequest[] = [];
  let listReads = 0;
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      if (request.method === 'PUT' && request.path.startsWith('/api/track-recipes/')) {
        return Promise.resolve(options.put ?? { status: 200, statusText: 'OK', body: RECIPE });
      }
      if (request.method === 'DELETE' && request.path.startsWith('/api/track-recipes/')) {
        return Promise.resolve(options.remove ?? { status: 204, statusText: 'No Content', body: null });
      }
      if (request.method === 'POST' && request.path === '/api/track-recipes') {
        return Promise.resolve(options.post ?? { status: 200, statusText: 'OK', body: RECIPE });
      }
      if (request.path === '/api/track-recipes') {
        const call = listReads;
        listReads += 1;
        if (options.recipeList !== undefined) return Promise.resolve(options.recipeList(call));
        return Promise.resolve({ status: 200, statusText: 'OK', body: options.recipes ?? [RECIPE] });
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

/* The recipe as the create resolved to it: a body the client never sent. */
const CREATED = {
  id: 'r-new', title: 'Ship checklist', body: 'Canonicalised by the server.\n',
  revision: 1, created_at: 3, updated_at: 3,
};

const OK = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

/** Compose a recipe and save it. Leaves the screen wherever the save put it. */
async function composeAndSave(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: 'New recipe' }));
  const field = await screen.findByRole('textbox', { name: BODY_FIELD });
  await user.type(field, 'What the author typed.');
  await user.click(screen.getByRole('button', { name: 'Save' }));
}

describe('the recipe editor', () => {
  /* Saving an untouched recipe must be a byte-exact echo, `if_revision` included. */
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

  /* The stub answers with a body the client never sent: the server's text must be
     on screen and the sent text not. */
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
    // Still editing: a conflict does not drop the reader back into the rendered view.
    expect(screen.getByRole('button', { name: 'Save' })).toBeTruthy();
  });

  /* A claim about the cache: reopening re-seeds the editor from the list's row, so
     a 409 must not leave the list holding the stale revision. */
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

  /* The editor seeds from its prop once and holds the row in state; a refetch
     landing mid-edit must not re-point the write at a revision the author never saw. */
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
  /* The mutation's invalidate only queues a refetch, so the stub's list read never
     returns the new row; only a page that renders the row the create resolved to passes. */
  it('renders the row the create resolved to, with no list refetch to lean on', async () => {
    const user = userEvent.setup();
    atRecipes({ recipeList: () => OK([]), post: OK(CREATED) });
    await composeAndSave(user);

    expect(await screen.findByText('Canonicalised by the server.')).toBeTruthy();
    expect(screen.queryByText('What the author typed.')).toBeNull();
    // Not dropped back to the list — which, with this stub, is empty.
    expect(screen.queryByText(/You have no recipes yet/)).toBeNull();
  });

  /* `trackRecipesQueryOptions` sets `retry: false`, so the cache keeps the previous
     list, the one without the new row. */
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
  /* `template_id` and `recipe_id` are mutually exclusive on the wire; the kernel
     answers a request naming both with a 400. */
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
