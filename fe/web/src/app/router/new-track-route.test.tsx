// @vitest-environment jsdom
// The new-track page: `/area/{id}/new`, reached from each Area group's `+`.
// `area_id` is the opener's area; the folder is optional and decides the whole
// request shape — no folder omits `cwd` *and* `attach_folder` (the kernel's
// managed default), a chosen folder sends both (#1147 S3).
//
// It lived in `app/shell/public.test.tsx` until #1211, because the shell owned
// a New track *dialog*. It owns nothing now: the `+` navigates, and the create
// belongs to `NewTrackRoute`. The file moved with the ownership rather than the
// shell keeping a suite about a surface it no longer has.
//
// This drives the real router, the real QueryClient and the real form — the
// wiring *is* the thing under test, and a fixture that re-implemented the
// branch would prove only that the fixture agrees with itself.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
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

/* The composer's accessible name: astryx puts `label` on the `contenteditable`
   as `aria-label`, so it resolves by label query. Spelled out here on purpose —
   losing it would make the field unreachable by screen reader and by voice
   control. */
const TASK_LABEL = 'What this track should do';

/* The folder chip's copy, restated for the same reason as `TASK_LABEL`: it is
   user-facing text, and a test that imported it from the component could not
   fail when the component silently changed it. Since #1211 the chip names the
   **default** rather than asking, and its accessible name says which control it
   is on top of that. */
const FOLDER_PLACEHOLDER = 'Neige workspace';
const FOLDER_CHIP_NAME = `Folder: ${FOLDER_PLACEHOLDER}`;

/* The template chip. It always names the current choice — "No template" until
   one is picked — so the name has one shape and the assertions vary the tail. */
const TEMPLATE_CHIP = /^Template: /;

const AREA = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const OTHER = { id: 'c2', name: 'Reading', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };

const LISTING = {
  path: '/srv/app', parent: '/srv', entries: [{ name: 'crates', is_dir: true }],
};

/* The created track, as the kernel returns it under #1211: **an empty title**.
   The client sends none, the kernel stores the empty string, and the planner agent
   names the track later through `calm.track.rename`. */
const TRACK_ROW = {
  id: 'w-new', area_id: 'c1', title: '', sort: 0, archived_at: null, pinned_at: null,
  lifecycle: 'draft', cwd: '/srv/managed', template_id: null, plugin_scope: null,
  purpose: null, template_input: null, terminal_at: null, created_at: 1, updated_at: 1,
};

/** The 409 `POST /api/tracks` answers a folder clash with — no `error` key. */
const CONFLICT = {
  folder_id: 4, area_id: 'c1', conflict_path: '/srv/app', conflict_kind: 'descendant',
};

/* #1209 — what `GET /api/track-templates` returns, in the two shapes that
   matter: one template bound to a running plugin (an `input_schema`, therefore
   fields) and one that is not. */
const TEMPLATES = [
  { id: 'small-change', title: 'Small change', tasks: [{ key: 'inspect', goal: 'Read the change.' }] },
  {
    id: 'issue-development',
    title: 'Issue development',
    input_schema: { type: 'object', required: ['issue_url', 'repo', 'issue_number'] },
    tasks: [{ key: 'inspect-issue', goal: 'Read the bound issue.' }],
  },
];

function harness(options: {
  templates?: unknown;
  areaDefaults?: Readonly<{ default_template_id: string | null; default_cwd: string | null }>;
  otherAreaDefaults?: Readonly<{ default_template_id: string | null; default_cwd: string | null }>;
  trackCreate?: ApiTransportResponse;
  /** Ordered create outcomes for retry/recovery paths that cross Area scope. */
  trackCreateSequence?: readonly ApiTransportResponse[];
  /** Override the detail read the track page makes when the create lands. */
  trackDetail?: ApiTransportResponse;
  /** Hold the detail read open until this resolves, to drive a slow landing. */
  heldDetail?: Promise<void>;
  /** Hold the create POST open until this resolves, to drive a late create. */
  heldCreate?: Promise<void>;
  /**
   * Hold `GET /api/areas` open until this resolves. The area list is what the
   * route consults to decide whether its `$areaId` still exists, so this is the
   * only way to render the page while that answer is genuinely unknown.
   */
  heldAreas?: Promise<void>;
  /**
   * Fail `GET /api/areas` outright. The read that answers "does this area still
   * exist" has a third state besides in-flight and landed, and a 500 leaves
   * `workspace.areas` at `[]` with `areasLoading` false — indistinguishable
   * from "landed, and this area is gone" to anything that only looks at the
   * list.
   */
  areasFail?: boolean;
  /**
   * Where the browser starts, under the basepath. Deep-linking is the entry
   * that reaches a *stale* area id: every in-app `+` can only name an area the
   * rail is currently showing.
   */
  path?: string;
} = {}) {
  const sent: ApiRequest[] = [];
  let trackCreateIndex = 0;
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      const posted = request.body as { area_id?: string } | undefined;
      if (request.method === 'POST' && request.path === '/api/tracks' && options.trackCreate) {
        return Promise.resolve(options.trackCreate);
      }
      if (request.method === 'POST' && request.path === '/api/tracks' && options.trackCreateSequence) {
        const response = options.trackCreateSequence[trackCreateIndex];
        trackCreateIndex += 1;
        if (response !== undefined) return Promise.resolve(response);
      }
      if (request.method === 'POST' && request.path === '/api/tracks' && options.heldCreate) {
        return options.heldCreate.then(() => ({
          status: 200,
          statusText: 'OK',
          body: { ...TRACK_ROW, area_id: posted?.area_id ?? 'c1' },
        }));
      }
      if (request.path === '/api/track-templates') {
        // `undefined` here is the read failing outright — the branch the
        // dialog must survive.
        const templates = options.templates;
        return templates === undefined
          ? Promise.resolve({ status: 500, statusText: 'Server Error', body: { message: 'boom' } })
          : Promise.resolve({ status: 200, statusText: 'OK', body: templates });
      }
      /* The track page reads the detail on arrival, and that read is where the
         planner card — the one the landing opens — comes from. Served here rather
         than left to fall through to `[]`, because a decode failure would look
         identical to "the feature did not run". The first message does not ride
         on this read: it went out on the create (#1299). */
      if (request.method === 'GET' && request.path === '/api/tracks/w-new') {
        if (options.trackDetail) return Promise.resolve(options.trackDetail);
        const detail = {
          status: 200,
          statusText: 'OK',
          body: {
            track: { ...TRACK_ROW },
            can_resume: false,
            cards: [{
              id: 'card-planner', track_id: 'w-new', kind: 'codex', title: 'Planner',
              payload: { planner_harness: true }, sort: 0, created_at: 1, updated_at: 1,
            }],
            overlays: [],
          },
        } satisfies ApiTransportResponse;
        /* Held open on request, so a test can land the track page's cards
           *after* the navigation has happened. Resolves to the same body
           either way. */
        return options.heldDetail
          ? options.heldDetail.then(() => detail)
          : Promise.resolve(detail);
      }
      if (request.path === '/api/areas' && options.areasFail) {
        return Promise.resolve({ status: 500, statusText: 'Server Error', body: { error: 'areas are unreadable' } });
      }
      if (request.path === '/api/areas' && options.heldAreas) {
        return options.heldAreas.then(() => ({ status: 200, statusText: 'OK', body: [AREA, OTHER] }));
      }
      const body = request.path === '/api/areas' ? [
        { ...AREA, ...options.areaDefaults }, { ...OTHER, ...options.otherAreaDefaults },
      ]
        : request.path.startsWith('/api/fs/listdir') ? LISTING
          : request.method === 'POST' && request.path === '/api/tracks'
            ? { ...TRACK_ROW, area_id: posted?.area_id ?? 'c1' }
            : [];
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  window.history.pushState({}, '', `${APP_BASEPATH}${options.path ?? '/'}`);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
  });
  /*
   * `StrictMode`, because production runs it (`app/auth/production-app.tsx`)
   * and because its absence here is what let the worst bug in this branch ship.
   *
   * React double-invokes effects in StrictMode — mount → cleanup → mount — and
   * a `useRef` latch written only in the cleanup arm ends up stuck on the
   * cleanup value from the very first render. That is exactly what happened to
   * `NewTrackRoute`'s `liveRef`: it latched `false` on mount, so *every* create
   * silently stopped navigating, and all 2117 jsdom tests stayed green because
   * this harness did not double-invoke. A real-kernel e2e caught it.
   *
   * Rendering under StrictMode makes that class visible where it is cheap to
   * see. Measured: with the `liveRef.current = true` arm removed, four cases in
   * this file fail; without StrictMode all fifteen pass.
   */
  render(
    <StrictMode>
      <QueryClientProvider client={client}>
        <ThemeProvider storage={memoryStorage()}>
          <RouterProvider router={router} />
        </ThemeProvider>
      </QueryClientProvider>
    </StrictMode>,
  );
  return { sent };
}

/*
 * Waits for the new-track page to be on screen and returns its composer.
 *
 * This replaces `findByRole('dialog', { name: 'New track' })`, which every case
 * used as "the surface is ready". The surface is a route now, so the thing to
 * wait for is the field itself — and waiting for it is still load-bearing for
 * the #1161 reason the dialog version gave: no click promises the next screen
 * synchronously, and every role query after this one depends on it.
 */
async function findComposer(): Promise<HTMLElement> {
  return screen.findByLabelText(TASK_LABEL);
}

/** Waits for the track page to be mounted — it is the surface that would redeem
 *  a leftover open request, so the assertion has to happen after it exists. */
async function findTrackPage(): Promise<HTMLElement> {
  return screen.findByRole('main');
}

/** What the composer currently holds. It is a `contenteditable`, not an input,
 *  so the value is its text rather than a `value` property. */
function composerText(): string {
  return screen.getByLabelText(TASK_LABEL).textContent ?? '';
}

/** The text of every first message delivered to the planner card (#1211). */
function plannerInputTexts(sent: readonly ApiRequest[]): unknown[] {
  return sent.filter((request) => request.method === 'POST' && request.path.endsWith('/planner/input'))
    .map((request) => (request.body as { text?: unknown } | undefined)?.text);
}

function createdTrackBodies(sent: readonly ApiRequest[]): unknown[] {
  return createdTrackRequests(sent).map((request) => request.body);
}

function createdTrackRequests(sent: readonly ApiRequest[]): ApiRequest[] {
  return sent.filter((request) => request.method === 'POST' && request.path === '/api/tracks');
}

describe('the new-track page is a route reached from Area groups', () => {
  it('carries the selected group into one shared create route', async () => {
    harness();
    // The rail's `+`, on an area the user is not currently inside: the whole
    // point of the row control is starting a track without navigating first.
    // It carries that area's id into the URL, which is what makes this one
    // route serve both openers.
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    expect(await findComposer()).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`);

    /* #1211 — and it is a *page*, so there is no modal over the app: the
       assertion that would catch a quiet return to a dialog. */
    expect(screen.queryByRole('dialog')).toBeNull();

    // Another group reaches the same route with its own Area id.
    window.history.back();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c1/new`);
  });

  it('remounts the route when switching directly between Areas and takes only the new Area defaults', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      areaDefaults: { default_template_id: 'small-change', default_cwd: '/srv/work-a' },
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/work-b' },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    expect(screen.getByRole('button', { name: 'Template: Small change' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Folder: /srv/work-a' })).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Draft for A');

    await userEvent.click(screen.getByRole('button', { name: 'New track in Reading' }));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`));
    expect(composerText()).toBe('');
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Folder: /srv/work-b' })).toBeTruthy();

    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Draft for B');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ area_id: 'c2', cwd: '/srv/work-b', attach_folder: true });
    expect(body).not.toHaveProperty('template_id');
  });

  it('does not let a create started in one Area navigate away after switching to another Area', async () => {
    let releaseCreate!: () => void;
    const heldCreate = new Promise<void>((resolve) => { releaseCreate = resolve; });
    const { sent } = harness({ templates: TEMPLATES, heldCreate });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Create in A');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));

    await userEvent.click(screen.getByRole('button', { name: 'New track in Reading' }));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`));
    releaseCreate();
    await act(async () => {
      await heldCreate;
      await new Promise((resolve) => { setTimeout(resolve, 0); });
    });
    await new Promise((resolve) => { setTimeout(resolve, 0); });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`);
  });

  /*
   * #1161's rule, carried onto the route: the caret starts in the field.
   *
   * In the dialog this was a missing `initialFocusRef` — opening focus went to
   * `focusables(panel)[0]`, the header's Close button, so a reader who opened
   * it and typed put nothing in the field, and **space activates a focused
   * button**, so the first space threw the dialog away. The route has no Close
   * button to lose focus to, but the failure it protects against is the same
   * and cheaper to reintroduce: arrive with focus on the document and every
   * keystroke goes nowhere.
   *
   * Kept as a behaviour assertion (type, then read the field back) rather than
   * only an `activeElement` check, because that is the thing the reader
   * actually notices.
   */
  it('arrives with the composer focused, so typing reaches it', async () => {
    harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await act(async () => { await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); }); });

    expect(document.activeElement).toBe(screen.getByLabelText(TASK_LABEL));

    await userEvent.keyboard('Read it');
    expect(composerText()).toBe('Read it');
  });

  /*
   * ── Nothing on this page names the track ─────────────────────────────────
   *
   * #1211 S2 deleted the title field, and S3 replaced the dialog with this
   * page. The field that survives is the composer, and it is the track's
   * *intent*, not its name — so the statement to keep alive across the move is
   * "no control here collects a title, and no `title` key is on the wire".
   *
   * Carried over from the shell suite's `creates with no title field on screen
   * and no title key on the wire`, which went with the dialog. The screen half
   * had to be rewritten: there, the absent field answered to the composer's own
   * label, and here that label is the composer.
   *
   * The body is asserted on **key absence**: `title: ''` reaches the same
   * stored value, so an assertion on the value could not tell "nobody named
   * this track" from "this client named it the empty string" — and only the
   * former leaves `calm.track.rename` able to name it (#1211 S1).
   */
  it('creates with no title control on screen and no title key on the wire', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    // Exposed first, so the absence checks below cannot pass vacuously.
    expect(await findComposer()).toBeTruthy();
    /* By name, not by counting textboxes: the composer is a textbox, and so is
       the template's input field on the issue-development path — a count would
       say nothing. Anything that named itself a title would match. */
    expect(screen.queryByRole('textbox', { name: /title/i })).toBeNull();
    expect(screen.queryByRole('combobox', { name: /title/i })).toBeNull();
    expect(screen.queryByLabelText(/title/i)).toBeNull();

    /* Create is gated on the sentence and on nothing else — no name is asked
       for, so nothing here can be blocking on one. (S2's dialog collected no
       sentence and its Create was live immediately; S3 makes the composer the
       page, so an empty one has nothing to submit.) */
    const create = await screen.findByRole('button', { name: 'Create track' });
    expect(create.hasAttribute('disabled')).toBe(true);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    expect(create.hasAttribute('disabled')).toBe(false);
    await userEvent.click(create);
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(Object.hasOwn(body, 'title')).toBe(false);
    expect(body).toMatchObject({ area_id: 'c2' });
  });

  it('posts the opener\'s area_id and omits cwd / attach_folder with no folder chosen', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    // #1161 — establish the page is on screen *and exposed* first. The
    // `queryByLabelText` absence check below would pass vacuously against a
    // page that never rendered, and `getByLabelText` does no accessibility
    // filtering, so it cannot stand in for this wait.
    expect(await findComposer()).toBeTruthy();
    expect(screen.queryByLabelText('Area')).toBeNull();
    /* #1147 S3 restated on top of #1209: the Folder control *is* here — this
       assertion used to be `toBeNull()` — and it starts empty. Empty is what
       "no folder chosen" looks like, and it is what the absence checks on the
       body below are the consequence of. */
    expect(screen.getByRole('button', { name: FOLDER_CHIP_NAME }).textContent).toBe(FOLDER_PLACEHOLDER);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ area_id: 'c2' });
    expect(body).toHaveProperty('theme');
    /* #1211 — the sentence is the track's *intent*, not its name. No `title` on
       the wire at all: the kernel stores the empty string and the planner agent
       renames later through `calm.track.rename`. The sentence rides on
       `first_message` instead (#1299, asserted just below) — a create that
       quietly went back to posting it as the title would satisfy neither. */
    expect(body).not.toHaveProperty('title');
    /* #1299 — and the sentence itself is on the create, under the key the
       kernel seeds into the harness-start transaction. Two halves, because they
       fail differently: posting it as something other than `first_message` is a
       400 the reader never asked for, and delivering it with a second write is
       the unsound three-write sequence this slice exists to have removed. */
    expect(body).toMatchObject({ first_message: 'Read it' });
    expect(plannerInputTexts(sent)).toEqual([]);
    // The managed-workspace branch is keyed on *absence*, not on a value:
    // `cwd: null` and `attach_folder: false` are both a different kernel path.
    expect(body).not.toHaveProperty('cwd');
    expect(body).not.toHaveProperty('attach_folder');
    expect(sent.some((request) => request.path.startsWith('/api/fs/listdir'))).toBe(false);
    // #1209 — Blank is the default, and Blank means the key is not on the wire
    // at all. `template_id: null` or `''` is a 400 from the kernel.
    expect(body).not.toHaveProperty('template_id');
    expect(body).not.toHaveProperty('template_input');
  });

  it('turns the Area defaults into an explicit template and attached-folder request', async () => {
    const message = '  Read it exactly  ';
    const { sent } = harness({
      templates: TEMPLATES,
      areaDefaults: { default_template_id: 'small-change', default_cwd: '/srv/ area ' },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Template: Small change' })).toBeTruthy();
    const folder = screen.getByRole('button', { name: 'Folder: /srv/ area' });
    expect(folder.getAttribute('aria-label')).toBe('Folder: /srv/ area ');
    await userEvent.type(screen.getByLabelText(TASK_LABEL), message);
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({
      area_id: 'c1',
      first_message: message,
      template_id: 'small-change',
      cwd: '/srv/ area ',
      attach_folder: true,
    });
  });

  it('lets one Track clear Area defaults back to a new managed folder', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      areaDefaults: { default_template_id: 'small-change', default_cwd: '/srv/area-default' },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^No template/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).not.toHaveProperty('template_id');
    expect(body).not.toHaveProperty('cwd');
    expect(body).not.toHaveProperty('attach_folder');
  });

  /*
   * #1299 — what the reader typed is what the agent is sent, byte for byte.
   *
   * The kernel forwards `first_message` to the agent untrimmed and hashes it
   * untrimmed, so the whitespace around the sentence is content, and this
   * route's only business with it is deciding whether the key rides at all.
   * Both layers used to trim — the form on the way out and this route on the
   * way to the wire — and a deliberately indented instruction went out
   * flattened with every suite green, because the form's own case asserted the
   * trimmed string.
   *
   * The assertion is on the *request body*, which is the only place the whole
   * path (composer → draft → POST) is visible at once: a trim reintroduced in
   * either layer fails here.
   */
  it('posts the sentence exactly as typed, whitespace and all', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    const padded = '  keep indentation  ';
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), padded);
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({ first_message: padded });
  });

  /*
   * The other half of the same contract. `attach_folder: true` is not decorative
   * — with it omitted the kernel refuses any path no area has already claimed,
   * so an attached create would 409 for exactly the folders a user is most
   * likely to pick. It is a no-op when this area already covers the path.
   */
  it('posts the picked folder as cwd with attach_folder: true', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');

    await userEvent.click(await screen.findByRole('button', { name: FOLDER_CHIP_NAME }));
    // The page owns this dialog: there is no outer one to push into, so
    // `DirectoryBrowser` is mounted in a `Dialog` of its own
    // (CAP-TRACKWORKSPACE-003). The child-view push is the *other* call site's
    // contract — `features/track/new-card`, which does render inside a dialog
    // (CAP-TRACKWORKSPACE-006). Named rather than counted, because naming is
    // what says which dialog this is.
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    expect(await findComposer()).toBeTruthy();

    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({
      area_id: 'c1', cwd: '/srv/app', attach_folder: true,
    });
    expect(sent.some((request) => request.path === '/api/fs/listdir')).toBe(true);
    // Attaching a folder is orthogonal to #1209's template choice: staying on
    // Blank must still keep `template_id` off the wire.
    expect(body).not.toHaveProperty('template_id');
  });

  /*
   * The two features on one request. The folder and the template are collected
   * by different controls and translated by different branches of
   * `the route's submit`, and nothing else proves the second spread does not
   * clobber the first.
   */
  it('carries a chosen folder and a chosen template on the same POST', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));

    await userEvent.click(await screen.findByRole('button', { name: FOLDER_CHIP_NAME }));
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({
      area_id: 'c1',
      template_id: 'small-change',
      cwd: '/srv/app',
      attach_folder: true,
    });
  });

  /*
   * The 409 body has no `error` key, so `ApiError.message` is the bare status
   * text: without decoding it the user is told "Conflict" and nothing else —
   * not which folder, not which area, not what to do instead.
   */
  it('renders the structured folder conflict, not the word Conflict', async () => {
    harness({
      templates: TEMPLATES,
      trackCreate: { status: 409, statusText: 'Conflict', body: CONFLICT },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    // The request, its rejection, and the re-render are three ticks the click
    // does not await; the default 1s window is not enough under a loaded suite.
    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    expect(alert.textContent).toContain('/srv/app');
    // `c1` is Work in the seeded area list — the id must never reach the page.
    expect(alert.textContent).toContain('area “Work”');
    expect(alert.textContent).not.toContain('c1');
    expect(alert.textContent).not.toBe('Conflict');
  });

  it.each(['equal', 'descendant'] as const)(
    'creates in the owning Area for a %s conflict without losing the draft',
    async (conflictKind) => {
    const { sent } = harness({
      templates: TEMPLATES,
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/app' },
      trackCreateSequence: [
        { status: 409, statusText: 'Conflict', body: { ...CONFLICT, conflict_kind: conflictKind } },
        { status: 201, statusText: 'Created', body: { ...TRACK_ROW, area_id: 'c1' } },
      ],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    await userEvent.click(within(alert).getByRole('button', { name: 'Create in Work' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const [failed, recovered] = createdTrackRequests(sent);
    expect(failed?.body).toMatchObject({
      area_id: 'c2', cwd: '/srv/app', attach_folder: true,
      first_message: 'Read it', template_id: 'small-change',
    });
    expect(recovered?.body).toMatchObject({
      area_id: 'c1', cwd: '/srv/app', attach_folder: true,
      first_message: 'Read it', template_id: 'small-change',
    });
    expect(recovered?.headers?.['Idempotency-Key']).toBeDefined();
    expect(recovered?.headers?.['Idempotency-Key']).not.toBe(failed?.headers?.['Idempotency-Key']);
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    },
  );

  it('does not offer an owning-Area retry for an ancestor conflict that moving cannot resolve', async () => {
    harness({
      templates: TEMPLATES,
      trackCreate: {
        status: 409,
        statusText: 'Conflict',
        body: { ...CONFLICT, conflict_kind: 'ancestor' },
      },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    expect(within(alert).queryByRole('button', { name: 'Create in Work' })).toBeNull();
  });

  it('uses the visible current draft when the owning-Area recovery is clicked', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/app' },
      trackCreateSequence: [
        { status: 409, statusText: 'Conflict', body: CONFLICT },
        { status: 201, statusText: 'Created', body: { ...TRACK_ROW, area_id: 'c1' } },
      ],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    const field = screen.getByLabelText(TASK_LABEL);
    await userEvent.type(field, 'Old message');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    await userEvent.clear(field);
    await userEvent.type(field, 'Current message');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^No template/ }));
    await userEvent.click(within(alert).getByRole('button', { name: 'Create in Work' }));

    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const recovered = createdTrackRequests(sent)[1];
    expect(recovered?.body).toMatchObject({ area_id: 'c1', first_message: 'Current message' });
    expect(recovered?.body).not.toHaveProperty('template_id');
    expect(recovered?.body).toMatchObject({ cwd: '/srv/app', attach_folder: true });
  });

  it('withdraws the owning-Area recovery when the conflicting folder changes', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/app' },
      trackCreateSequence: [
        { status: 409, statusText: 'Conflict', body: CONFLICT },
        { status: 201, statusText: 'Created', body: { ...TRACK_ROW, area_id: 'c2' } },
      ],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    expect(within(alert).getByRole('button', { name: 'Create in Work' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
    expect(within(alert).queryByRole('button', { name: 'Create in Work' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const retried = createdTrackRequests(sent)[1];
    expect(retried?.body).toMatchObject({ area_id: 'c2', first_message: 'Read it' });
    expect(retried?.body).not.toHaveProperty('cwd');
    expect(retried?.body).not.toHaveProperty('attach_folder');
  });

  /*
   * #1209, through the shell rather than the form: the form builds a draft,
   * but only this wiring decides what reaches the wire. A form-level test
   * cannot see `the route's submit` dropping a field on the way to the POST.
   */
  it('carries the chosen template onto the create POST', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Fix the thing');
    /* `findBy`: the picker's trigger is there from the first paint, but the
       option only exists once the template read has landed — so the wait is on
       the option inside the opened menu, not on the trigger. */
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Issue development/ }));
    await userEvent.type(
      screen.getByLabelText('Issue URL'),
      'https://github.com/keanji-x/neige-calm/issues/1209',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({
      area_id: 'c2',
      template_id: 'issue-development',
      template_input: {
        issue_url: 'https://github.com/keanji-x/neige-calm/issues/1209',
        repo: 'keanji-x/neige-calm',
        issue_number: 1209,
        merge_policy: 'hold-for-ratify',
      },
    });
  });

  it('sends an unbound template as an id with no template_input', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Tiny fix');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ template_id: 'small-change', first_message: 'Tiny fix' });
    expect(body).not.toHaveProperty('title');
    expect(plannerInputTexts(sent)).toEqual([]);
    expect(body).not.toHaveProperty('template_input');
  });

  /*
   * The real failure mode, driven end to end: the template read 500s and the
   * app's only track-creation entry point still creates a track. Asserted here
   * and not only on the form because the degradation lives in the wiring —
   * `data ?? []` plus a query that does not retry.
   */
  it('still creates a track when the template read fails outright', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    /* Wait for the failure to *land*, not for the request to leave. Waiting on
       `sent` only proves the query started: react-query could still be pending
       when the submit runs, and then this case would silently be testing
       "submits while the list is loading" — a different, easier branch.
       The rendered notice is the first observable moment the 500 has been
       consumed (`useTrackTemplates` turns `isError` into this string), so it is
       what the wait is on. */
    await screen.findByText(/Could not load templates/);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it anyway');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({
      area_id: 'c2', first_message: 'Read it anyway',
    });
    expect(createdTrackBodies(sent)[0]).not.toHaveProperty('title');
    expect(plannerInputTexts(sent)).toEqual([]);
  });
});

/*
 * A deep link is the only way to reach this route with an area id the rail
 * cannot show: the `+` controls are rendered from the area list itself. The id
 * that outlives its area is the interesting one — a stale bookmark, a link
 * pasted after the area was deleted — because it is *syntactically* fine, so
 * without a lookup the page renders a composer that works right up until the
 * create eats a 4xx.
 */
describe('the route refuses an area id that no longer exists', () => {
  /*
   * ── One case per state of `GET /api/areas`, and each falsifiable alone ────
   *
   * The composer is what a stale id must never reach, so "no composer" is the
   * shared half; what differs is which state produces it and what is shown
   * instead. The three route branches (in flight → nothing, failed → the
   * rail's own error, settled → the existence verdict) are mutated one at a
   * time in review, and each mutation turns exactly one of these four red —
   * which is the property the earlier three-case shape did not have: its
   * "an area that does exist" case passed off the *loading* composer, so
   * forcing the existence check to `false` left it green.
   */
  it('reports a deleted area instead of rendering a working composer', async () => {
    harness({ templates: TEMPLATES, path: '/area/c9/new' });
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('This area could not be found.');
    /* The composer's absence is the point: an ErrorBox rendered *above* a live
       form would still lose the reader's sentence to the 4xx. */
    expect(screen.queryByLabelText(TASK_LABEL)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Create track' })).toBeNull();
  });

  /*
   * The list has to have *landed* before the composer is allowed on screen —
   * asserted in that order, not just at the end.
   *
   * A route that renders the form while the read is in flight would satisfy a
   * bare `findComposer()` at the end of this case (the list lands either way),
   * so the load-bearing assertion is the one before `releaseAreas()`: at that
   * moment nothing is known about `c1` and there must be nothing to type into.
   * Only the composer is asserted absent there, so that this case answers for
   * the *success* branch and the in-flight case below answers for the
   * in-flight one.
   */
  it('renders the composer for an area that does exist, and only once the list has landed', async () => {
    let releaseAreas = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseAreas = () => { resolve(); }; });
    harness({ templates: TEMPLATES, path: '/area/c1/new', heldAreas: held });

    // The route is mounted and the read is outstanding — nothing submittable.
    await screen.findByRole('button', { name: 'Today' });
    expect(screen.queryByLabelText(TASK_LABEL)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Create track' })).toBeNull();

    releaseAreas();
    await held;
    expect(await findComposer()).toBeTruthy();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  /*
   * In flight is not a verdict in *either* direction.
   *
   * `workspace.areas` is `[]` while `GET /api/areas` is in flight, and `[]`
   * contains no id — so a bare `some()` calls every area deleted for as long as
   * the read takes. The first fix for that let the in-flight state fall through
   * to the form instead, which is the worse half of the same mistake: a cold
   * deep link got a *submittable* composer for an id nothing had confirmed, and
   * the reader's sentence went to the 4xx anyway.
   *
   * So neither the composer nor the verdict may appear while the read is open.
   * The read is never released into a settled assertion here — that half is the
   * two cases above — so mutating the existence or failure branch leaves this
   * one green.
   */
  it('renders neither a composer nor a verdict while the area list is still loading', async () => {
    let releaseAreas = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseAreas = () => { resolve(); }; });
    harness({ templates: TEMPLATES, path: '/area/c9/new', heldAreas: held });

    /* The shell is up and the route committed: without this the absence checks
       below would pass against an app that had not rendered at all. */
    await screen.findByRole('button', { name: 'Today' });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c9/new`);

    expect(screen.queryByLabelText(TASK_LABEL)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Create track' })).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();

    // Let the read finish so nothing is left hanging on the way out.
    releaseAreas();
    await held;
  });

  /*
   * A failed read is not a landed one.
   *
   * `areas` falls back to `[]` on failure too, and `areasLoading` is false by
   * then — so a check that only asks "has loading stopped" reads a 500 as
   * "the list arrived and this area is not in it" for a deleted-looking id, and
   * as "carry on" for every other id, permanently. Neither is true: the
   * question has no answer, so the page says what actually went wrong, in the
   * rail's own words, with the rail's own retry.
   */
  it('reports a failed area read instead of a composer or a deletion verdict', async () => {
    harness({ templates: TEMPLATES, path: '/area/c1/new', areasFail: true });

    /* Scoped to `main`, because the rail reports the same failed read in its
       own ErrorBox: an unscoped `getByRole('alert')` would pass on the rail's
       copy no matter what this route decided. And `waitFor` rather than a
       single read, so the assertion is about where the page *settles* — the
       failure needs a tick to land, and what is on screen before it does is
       the in-flight case's business, not this one's. */
    const main = await screen.findByRole('main');
    await waitFor(() => {
      expect(within(main).getByRole('alert').textContent).toContain('areas are unreadable');
    });
    // Not the deletion wording: the server never said this area is gone.
    expect(within(main).getByRole('alert').textContent).not.toContain('This area could not be found.');
    expect(within(main).getByRole('button', { name: 'Retry' })).toBeTruthy();
    expect(screen.queryByLabelText(TASK_LABEL)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Create track' })).toBeNull();
  });
});

/*
 * Delivery rides on the create (#1299) — see `NewTrackRoute`'s doc.
 *
 * The failure matrix that used to live here drove a three-write sequence. Both
 * review channels showed the sequence cannot be made sound from a component (an
 * unmount mid-flight loses the sentence silently, and `/planner/input` has no
 * idempotency key so any retry can double-send), so the write moved into
 * `POST /api/tracks` and the tests moved with it.
 *
 * What is asserted here is that shape and its counterpart: the sentence goes
 * out once, on the create, by no other route — and the reader still lands with
 * the planner conversation open, which is now where the answer arrives.
 */
describe('the sentence is delivered by the create, and the track opens on it', () => {
  it('sends the sentence once, on the create, and by no second write', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    expect(createdTrackBodies(sent)[0]).toMatchObject({ first_message: 'Read it' });
    expect(createdTrackRequests(sent)[0]?.headers?.['Idempotency-Key']).toBeDefined();
    /*
     * Exactly once, and written so it says that rather than "at least once".
     *
     * The `waitFor` above returns the instant the first create is seen, so a
     * second one emitted a tick later would still be in flight when it
     * resolved. The landing that follows is the settle window: by the time the
     * track page has mounted, every continuation this create started has run,
     * and *then* the count is read. The create carries one draft-scoped
     * `Idempotency-Key`; this count still guards against a second POST because
     * automatic retries are a separate policy from server-side replay safety.
     */
    await findTrackPage();
    expect(createdTrackBodies(sent)).toHaveLength(1);
    /* And by no other route: no `/planner/input` went out, so nothing can be
       half-delivered — if someone re-adds the three-write sequence here on top
       of the create, this fails and sends them back to `NewTrackRoute`'s doc. */
    expect(plannerInputTexts(sent)).toEqual([]);
  });

  /*
   * The other half of the landing: the create states `openPlanner` on the
   * navigation it makes and the track route body redeems it against its own
   * cards, so the reader arrives looking at the conversation their sentence was
   * just delivered into — and with the caret where the reply is answered.
   *
   * Asserted through the drawer the track page opens, not by spying on the
   * router state: the marker is an implementation detail and the drawer is the
   * thing the reader gets. Without this case the `openPlanner: true` on the `go()`
   * below could be deleted and every other case here would stay green.
   */
  it('opens the track\'s planner conversation on arrival', async () => {
    harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    /* Named, not by bare role: the track page's panel column is a
       `complementary` too, so the role alone matches two elements. `Drawer`
       names itself from the conversation's title, and the planner card's is
       "Planner". */
    expect(await screen.findByRole('complementary', { name: 'Planner' })).toBeTruthy();
  });

  /*
   * ── #1449 ──────────────────────────────────────────────────────────────────
   *
   * And the sentence is *on* that conversation when the reader arrives.
   *
   * The create delivered it, but delivered is not readable: a transcript is
   * read from one persisted table (`crates/calm-truth/src/db/sqlite/read.rs`)
   * and rows land there only when codex echoes the turn back
   * (`crates/calm-server/src/harness/run_loop.rs`). This harness serves `[]`
   * for the item read, which is what the kernel really answers in that window,
   * so the only thing that can put the words on screen is the optimistic echo
   * the landing mints — and the only way that echo can name the right card is
   * by riding the history entry, since `POST /api/tracks` answers with a
   * `Track` and the planner card id arrives a route later.
   */
  it('shows the sentence that made the track before the server echoes it', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    const drawer = await screen.findByRole('complementary', { name: 'Planner' });
    await waitFor(() => expect(
      [...drawer.querySelectorAll('[data-nc-turn="you"]')].map((turn) => turn.textContent),
    ).toEqual(['Read it']));
    expect(drawer.querySelector('[data-nc-thread-empty]')).toBeNull();
    /* With zero server items — the read happened and answered nothing, so the
       words on screen came from the echo and from nowhere else. */
    const items = sent.filter((request) => request.path.includes('/harness/items'));
    expect(items.length).toBeGreaterThan(0);
    expect(plannerInputTexts(sent)).toEqual([]);
  });

  /*
   * A slow landing must not hold the reader on the form.
   *
   * The create used to read the track detail *here*, race it against a deadline,
   * and write the planner card's id into the conversation registry before
   * navigating. The registry outlives every route, so a landing that never
   * reached the track left that request standing and sprang a drawer open on a
   * later visit — which is why the intent moved onto the history entry
   * (`openPlanner`) and the read moved to the track page that owns it.
   *
   * So the guarantee here is now two-sided and this case pins both: the
   * navigation does not wait on any read, and the drawer opens when the read it
   * does depend on finally lands — on this entry, and only this one.
   */
  it('navigates before the track detail lands, and opens the drawer when it does', async () => {
    let releaseDetail = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseDetail = () => { resolve(); }; });
    const { sent } = harness({ heldDetail: held });

    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    // The reader goes at once — nothing on this page is waiting for the read.
    await waitFor(() => { expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`); });
    expect(createdTrackBodies(sent)).toHaveLength(1);
    await findTrackPage();
    expect(screen.queryByRole('complementary', { name: 'Planner' })).toBeNull();

    releaseDetail();
    await held;
    expect(await screen.findByRole('complementary', { name: 'Planner' })).toBeTruthy();
  }, 10_000);

  /*
   * A create that lands after the reader has moved on must not yank them back.
   *
   * `POST /api/tracks` can be slow, and nothing stops them pressing Back or
   * picking a rail row while it is in flight. The route unmounts but the
   * promise continuation still runs, and an unguarded `go()` pulled them off
   * the page they had just chosen. The track is created either way and is in the
   * rail; being navigated costs them their own last action.
   */
  it('does not yank the reader back when the create lands after they left', async () => {
    let releaseCreate = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseCreate = () => { resolve(); }; });
    const { sent } = harness({ heldCreate: held });

    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    // They leave while the create is still in flight.
    await userEvent.click(await screen.findByRole('button', { name: 'Today' }));
    await waitFor(() => { expect(window.location.pathname).toBe(`${APP_BASEPATH}/`); });

    releaseCreate();
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    // Still where they chose to be.
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/`);

    /*
     * And nothing was written into the registry on the way past.
     *
     * Asserting only the pathname above is not enough, and that gap is exactly
     * what review caught by execution on the shape this replaced: a drawer
     * request written into a provider after the reader left was invisible
     * *here* and surfaced on their **next** visit to the track as a Planner drawer
     * nobody opened. The intent now rides on the history entry the create would
     * have made — and it never made one — so the later visit is the observable
     * that says so, and it stays the observable regardless of how the intent is
     * carried.
     */
    window.history.pushState({}, '', `${APP_BASEPATH}/track/w-new`);
    /* Wait for the page to be far enough along that a leftover request *would*
       have been redeemed — the title only renders once the track detail has
       landed, which is the same read the drawer needs. Asserting absence before
       that is asserting that nothing has happened yet, which is true either
       way; the first two attempts at this test both failed that way. */
    await screen.findByRole('button', { name: 'Rename track' });
    expect(screen.queryByRole('complementary', { name: 'Planner' })).toBeNull();
  }, 10_000);

  /* A track detail that will not load costs a closed drawer and nothing else, so
     it must not block the navigation — the sentence went out on the create, and
     nothing about its delivery rides on this read. */
  it('still lands on the track when the track detail read fails', async () => {
    const { sent } = harness({
      trackDetail: { status: 500, statusText: 'Server Error', body: { error: 'boom' } },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
  });

  /* Not "and creates nothing": a failed create is not a create that did not
     happen. `first_message` makes a failed harness start answer 500 *after*
     the track is minted (#1299), so what this route owes the reader on a
     failure is the report, their text back, and no automatic retry — the state
     of the server is the kernel's to say, not this test's. */
  it('reports a create that failed, keeps the sentence, and does not automatically retry', async () => {
    const { sent } = harness({
      trackCreate: { status: 500, statusText: 'Server Error', body: { error: 'boom' } },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    expect(await screen.findByRole('alert')).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`);
    expect(composerText()).toBe('Read it');
    /* One attempt, and no automatic second one on the reader's behalf. */
    expect(createdTrackBodies(sent)).toHaveLength(1);
    expect(plannerInputTexts(sent)).toEqual([]);

    /* An explicit retry is the same draft and therefore the same key. */
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const [first, retry] = createdTrackRequests(sent);
    expect(retry?.headers?.['Idempotency-Key']).toBe(first?.headers?.['Idempotency-Key']);
  });

  it('rekeys an exhausted draft before the reader explicitly retries it', async () => {
    const { sent } = harness({
      trackCreate: {
        status: 409,
        statusText: 'Conflict',
        body: { error: 'this key is used up', code: 'idempotency_key_exhausted' },
      },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    expect((await screen.findByRole('alert')).textContent).toContain('this key is used up');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const [exhausted, fresh] = createdTrackRequests(sent);
    expect(fresh?.headers?.['Idempotency-Key']).toBeDefined();
    expect(fresh?.headers?.['Idempotency-Key'])
      .not.toBe(exhausted?.headers?.['Idempotency-Key']);
    expect(createdTrackBodies(sent)).toEqual([
      expect.objectContaining({ first_message: 'Read it' }),
      expect.objectContaining({ first_message: 'Read it' }),
    ]);
  });

  it.each([
    ['payload conflict', 'already used with different payload'],
    ['legacy unprovable key', 'this key predates durable request fingerprints'],
  ])('does not silently rekey a %s', async (_case, errorMessage) => {
    const { sent } = harness({
      trackCreate: {
        status: 409,
        statusText: 'Conflict',
        body: { error: errorMessage, code: 'conflict' },
      },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    expect((await screen.findByRole('alert')).textContent).toContain(errorMessage);
    expect(screen.getByRole('button', { name: 'Start as a new track' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const [conflict, retry] = createdTrackRequests(sent);
    expect(retry?.headers?.['Idempotency-Key']).toBe(conflict?.headers?.['Idempotency-Key']);

    await userEvent.click(await screen.findByRole('button', { name: 'Start as a new track' }));
    expect(screen.queryByRole('alert')).toBeNull();
    expect(createdTrackRequests(sent)).toHaveLength(2);
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(3));
    const explicitNew = createdTrackRequests(sent)[2];
    expect(explicitNew?.headers?.['Idempotency-Key']).toBeDefined();
    expect(explicitNew?.headers?.['Idempotency-Key'])
      .not.toBe(conflict?.headers?.['Idempotency-Key']);
  });
});
