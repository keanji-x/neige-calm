// @vitest-environment jsdom
// The new-wave page: `/cove/{id}/new`, reached from two `+` entry points.
// `cove_id` is the opener's cove; the folder is optional and decides the whole
// request shape — no folder omits `cwd` *and* `attach_folder` (the kernel's
// managed default), a chosen folder sends both (#1147 S3).
//
// It lived in `app/shell/public.test.tsx` until #1211, because the shell owned
// a New wave *dialog*. It owns nothing now: the `+` navigates, and the create
// belongs to `NewWaveRoute`. The file moved with the ownership rather than the
// shell keeping a suite about a surface it no longer has.
//
// This drives the real router, the real QueryClient and the real form — the
// wiring *is* the thing under test, and a fixture that re-implemented the
// branch would prove only that the fixture agrees with itself.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
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
const TASK_LABEL = 'What this wave should do';

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

const COVE = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const OTHER = { id: 'c2', name: 'Reading', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };

const LISTING = {
  path: '/srv/app', parent: '/srv', entries: [{ name: 'crates', is_dir: true }],
};

/* The created wave, as the kernel returns it under #1211: **an empty title**.
   The client sends none, the kernel stores the empty string, and the spec agent
   names the wave later through `calm.wave.rename`. */
const WAVE_ROW = {
  id: 'w-new', cove_id: 'c1', title: '', sort: 0, archived_at: null, pinned_at: null,
  lifecycle: 'draft', cwd: '/srv/managed', template_id: null, plugin_scope: null,
  purpose: null, template_input: null, terminal_at: null, created_at: 1, updated_at: 1,
};

/** The 409 `POST /api/waves` answers a folder clash with — no `error` key. */
const CONFLICT = {
  folder_id: 4, cove_id: 'c1', conflict_path: '/srv/app', conflict_kind: 'descendant',
};

/* #1209 — what `GET /api/wave-templates` returns, in the two shapes that
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
  waveCreate?: ApiTransportResponse;
  /** Override the detail read the wave page makes when the create lands. */
  waveDetail?: ApiTransportResponse;
  /** Hold the detail read open until this resolves, to drive a slow landing. */
  heldDetail?: Promise<void>;
  /** Hold the create POST open until this resolves, to drive a late create. */
  heldCreate?: Promise<void>;
} = {}) {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      const posted = request.body as { cove_id?: string } | undefined;
      if (request.method === 'POST' && request.path === '/api/waves' && options.waveCreate) {
        return Promise.resolve(options.waveCreate);
      }
      if (request.method === 'POST' && request.path === '/api/waves' && options.heldCreate) {
        return options.heldCreate.then(() => ({
          status: 200,
          statusText: 'OK',
          body: { ...WAVE_ROW, cove_id: posted?.cove_id ?? 'c1' },
        }));
      }
      if (request.path === '/api/wave-templates') {
        // `undefined` here is the read failing outright — the branch the
        // dialog must survive.
        const templates = options.templates;
        return templates === undefined
          ? Promise.resolve({ status: 500, statusText: 'Server Error', body: { message: 'boom' } })
          : Promise.resolve({ status: 200, statusText: 'OK', body: templates });
      }
      /* The wave page reads the detail on arrival, and that read is where the
         spec card — the one the landing opens — comes from. Served here rather
         than left to fall through to `[]`, because a decode failure would look
         identical to "the feature did not run". No first message rides on it —
         that is #1299. */
      if (request.method === 'GET' && request.path === '/api/waves/w-new') {
        if (options.waveDetail) return Promise.resolve(options.waveDetail);
        const detail = {
          status: 200,
          statusText: 'OK',
          body: {
            wave: { ...WAVE_ROW },
            cards: [{
              id: 'card-spec', wave_id: 'w-new', kind: 'codex', title: 'Spec',
              payload: { spec_harness: true }, sort: 0, created_at: 1, updated_at: 1,
            }],
            overlays: [],
          },
        } satisfies ApiTransportResponse;
        /* Held open on request, so a test can land the wave page's cards
           *after* the navigation has happened. Resolves to the same body
           either way. */
        return options.heldDetail
          ? options.heldDetail.then(() => detail)
          : Promise.resolve(detail);
      }
      const body = request.path === '/api/coves' ? [COVE, OTHER]
        : request.path.startsWith('/api/fs/listdir') ? LISTING
          : request.method === 'POST' && request.path === '/api/waves'
            ? { ...WAVE_ROW, cove_id: posted?.cove_id ?? 'c1' }
            : [];
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  window.history.pushState({}, '', `${APP_BASEPATH}/cove/c1`);
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
   * `NewWaveRoute`'s `liveRef`: it latched `false` on mount, so *every* create
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
 * Waits for the new-wave page to be on screen and returns its composer.
 *
 * This replaces `findByRole('dialog', { name: 'New wave' })`, which every case
 * used as "the surface is ready". The surface is a route now, so the thing to
 * wait for is the field itself — and waiting for it is still load-bearing for
 * the #1161 reason the dialog version gave: no click promises the next screen
 * synchronously, and every role query after this one depends on it.
 */
async function findComposer(): Promise<HTMLElement> {
  return screen.findByLabelText(TASK_LABEL);
}

/** Waits for the wave page to be mounted — it is the surface that would redeem
 *  a leftover open request, so the assertion has to happen after it exists. */
async function findWavePage(): Promise<HTMLElement> {
  return screen.findByRole('main');
}

/** What the composer currently holds. It is a `contenteditable`, not an input,
 *  so the value is its text rather than a `value` property. */
function composerText(): string {
  return screen.getByLabelText(TASK_LABEL).textContent ?? '';
}

/** The text of every first message delivered to the spec card (#1211). */
function specInputTexts(sent: readonly ApiRequest[]): unknown[] {
  return sent.filter((request) => request.method === 'POST' && request.path.endsWith('/spec/input'))
    .map((request) => (request.body as { text?: unknown } | undefined)?.text);
}

function createdWaveBodies(sent: readonly ApiRequest[]): unknown[] {
  return sent.filter((request) => request.method === 'POST' && request.path === '/api/waves')
    .map((request) => request.body);
}

describe('the new-wave page is a route, and both `+` entry points navigate to it', () => {
  it('lands on the same page from the rail and from the cove page', async () => {
    harness();
    // The rail's `+`, on a cove the user is not currently inside: the whole
    // point of the row control is starting a wave without navigating first.
    // It carries that cove's id into the URL, which is what makes this one
    // route serve both openers.
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    expect(await findComposer()).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/cove/c2/new`);

    /* #1211 — and it is a *page*, so there is no modal over the app: the
       assertion that would catch a quiet return to a dialog. */
    expect(screen.queryByRole('dialog')).toBeNull();

    // The cove page's WAVES module head reaches the same route for the cove
    // the reader is inside — one page, one set of strings, two openers.
    window.history.back();
    await userEvent.click(await screen.findByRole('button', { name: 'New wave' }));
    expect(await findComposer()).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/cove/c1/new`);
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
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    await act(async () => { await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); }); });

    expect(document.activeElement).toBe(screen.getByLabelText(TASK_LABEL));

    await userEvent.keyboard('Read it');
    expect(composerText()).toBe('Read it');
  });

  /*
   * ── Nothing on this page names the wave ─────────────────────────────────
   *
   * #1211 S2 deleted the title field, and S3 replaced the dialog with this
   * page. The field that survives is the composer, and it is the wave's
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
   * this wave" from "this client named it the empty string" — and only the
   * former leaves `calm.wave.rename` able to name it (#1211 S1).
   */
  it('creates with no title control on screen and no title key on the wire', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
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
    const create = await screen.findByRole('button', { name: 'Create wave' });
    expect(create.hasAttribute('disabled')).toBe(true);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    expect(create.hasAttribute('disabled')).toBe(false);
    await userEvent.click(create);
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(Object.hasOwn(body, 'title')).toBe(false);
    expect(body).toMatchObject({ cove_id: 'c2' });
  });

  it('posts the opener\'s cove_id and omits cwd / attach_folder with no folder chosen', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    // #1161 — establish the page is on screen *and exposed* first. The
    // `queryByLabelText` absence check below would pass vacuously against a
    // page that never rendered, and `getByLabelText` does no accessibility
    // filtering, so it cannot stand in for this wait.
    expect(await findComposer()).toBeTruthy();
    expect(screen.queryByLabelText('Cove')).toBeNull();
    /* #1147 S3 restated on top of #1209: the Folder control *is* here — this
       assertion used to be `toBeNull()` — and it starts empty. Empty is what
       "no folder chosen" looks like, and it is what the absence checks on the
       body below are the consequence of. */
    expect(screen.getByRole('button', { name: FOLDER_CHIP_NAME }).textContent).toBe(FOLDER_PLACEHOLDER);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ cove_id: 'c2' });
    expect(body).toHaveProperty('theme');
    /* #1211 — the sentence is the wave's *intent*, not its name. No `title` on
       the wire at all: the kernel stores the empty string and the spec agent
       renames later through `calm.wave.rename`. The sentence itself is not on
       the wire either yet (#1299, asserted just below) — a create that quietly
       went back to posting it as the title would satisfy neither. */
    expect(body).not.toHaveProperty('title');
    /* #1299 — nothing is delivered from this page yet. */
    expect(specInputTexts(sent)).toEqual([]);
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

  /*
   * The other half of the same contract. `attach_folder: true` is not decorative
   * — with it omitted the kernel refuses any path no cove has already claimed,
   * so an attached create would 409 for exactly the folders a user is most
   * likely to pick. It is a no-op when this cove already covers the path.
   */
  it('posts the picked folder as cwd with attach_folder: true', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');

    await userEvent.click(await screen.findByRole('button', { name: FOLDER_CHIP_NAME }));
    // The page owns this dialog: there is no outer one to push into, so
    // `DirectoryBrowser` is mounted in a `Dialog` of its own
    // (CAP-WAVEWORKSPACE-003). The child-view push is the *other* call site's
    // contract — `features/wave/new-card`, which does render inside a dialog
    // (CAP-WAVEWORKSPACE-006). Named rather than counted, because naming is
    // what says which dialog this is.
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    expect(await findComposer()).toBeTruthy();

    await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({
      cove_id: 'c1', cwd: '/srv/app', attach_folder: true,
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
    await userEvent.click(await screen.findByRole('button', { name: 'New wave' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));

    await userEvent.click(await screen.findByRole('button', { name: FOLDER_CHIP_NAME }));
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));

    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({
      cove_id: 'c1',
      template_id: 'small-change',
      cwd: '/srv/app',
      attach_folder: true,
    });
  });

  /*
   * The 409 body has no `error` key, so `ApiError.message` is the bare status
   * text: without decoding it the user is told "Conflict" and nothing else —
   * not which folder, not which cove, not what to do instead.
   */
  it('renders the structured folder conflict, not the word Conflict', async () => {
    harness({
      templates: TEMPLATES,
      waveCreate: { status: 409, statusText: 'Conflict', body: CONFLICT },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
    // The request, its rejection, and the re-render are three ticks the click
    // does not await; the default 1s window is not enough under a loaded suite.
    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    expect(alert.textContent).toContain('/srv/app');
    // `c1` is Work in the seeded cove list — the id must never reach the page.
    expect(alert.textContent).toContain('cove “Work”');
    expect(alert.textContent).not.toContain('c1');
    expect(alert.textContent).not.toBe('Conflict');
  });

  /*
   * #1209, through the shell rather than the form: the form builds a draft,
   * but only this wiring decides what reaches the wire. A form-level test
   * cannot see `the route's submit` dropping a field on the way to the POST.
   */
  it('carries the chosen template onto the create POST', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
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
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({
      cove_id: 'c2',
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
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Tiny fix');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ template_id: 'small-change' });
    expect(body).not.toHaveProperty('title');
    expect(specInputTexts(sent)).toEqual([]);
    expect(body).not.toHaveProperty('template_input');
  });

  /*
   * The real failure mode, driven end to end: the template read 500s and the
   * app's only wave-creation entry point still creates a wave. Asserted here
   * and not only on the form because the degradation lives in the wiring —
   * `data ?? []` plus a query that does not retry.
   */
  it('still creates a wave when the template read fails outright', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    /* Wait for the failure to *land*, not for the request to leave. Waiting on
       `sent` only proves the query started: react-query could still be pending
       when the submit runs, and then this case would silently be testing
       "submits while the list is loading" — a different, easier branch.
       The rendered notice is the first observable moment the 500 has been
       consumed (`useWaveTemplates` turns `isError` into this string), so it is
       what the wait is on. */
    await screen.findByText(/Could not load templates/);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it anyway');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({ cove_id: 'c2' });
    expect(createdWaveBodies(sent)[0]).not.toHaveProperty('title');
    expect(specInputTexts(sent)).toEqual([]);
  });
});

/*
 * Delivery is not this page's job yet (#1299) — see `NewWaveRoute`'s doc.
 *
 * The failure matrix that used to live here drove the three-write sequence.
 * Both review channels showed the sequence cannot be made sound from a
 * component (an unmount mid-flight loses the sentence silently, and
 * `/spec/input` has no idempotency key so any retry can double-send), so the
 * write moves into `POST /api/waves` under #1299 and the tests move with it.
 *
 * What is left is the property this slice does promise, and its counterpart:
 * the wave is created, nothing is sent, and the reader lands with the spec
 * conversation open so they can say it there.
 */
describe('the sentence is not delivered yet, and the wave opens ready for it', () => {
  it('creates the wave and sends no first message', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));

    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w-new`));
    /* The assertion that keeps this slice honest in both directions: no
       `/spec/input` went out, so nothing can be half-delivered — and if someone
       re-adds delivery here without moving it into the create, this fails and
       sends them to #1299. */
    expect(specInputTexts(sent)).toEqual([]);
  });

  /*
   * The positive half: the create states `openSpec` on the navigation it makes
   * and the wave route body redeems it against its own cards, so the reader
   * lands ready to say the sentence again.
   *
   * Asserted through the drawer the wave page opens, not by spying on the
   * router state: the marker is an implementation detail and the drawer is the
   * thing the reader gets. Without this case the `openSpec: true` on the `go()`
   * below could be deleted and every other case here would stay green.
   */
  it('opens the wave\'s spec conversation on arrival', async () => {
    harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));

    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w-new`));
    /* Named, not by bare role: the wave page's panel column is a
       `complementary` too, so the role alone matches two elements. `Drawer`
       names itself from the conversation's title, and the spec card's is
       "Spec". */
    expect(await screen.findByRole('complementary', { name: 'Spec' })).toBeTruthy();
  });

  /*
   * A slow landing must not hold the reader on the form.
   *
   * The create used to read the wave detail *here*, race it against a deadline,
   * and write the spec card's id into the conversation registry before
   * navigating. The registry outlives every route, so a landing that never
   * reached the wave left that request standing and sprang a drawer open on a
   * later visit — which is why the intent moved onto the history entry
   * (`openSpec`) and the read moved to the wave page that owns it.
   *
   * So the guarantee here is now two-sided and this case pins both: the
   * navigation does not wait on any read, and the drawer opens when the read it
   * does depend on finally lands — on this entry, and only this one.
   */
  it('navigates before the wave detail lands, and opens the drawer when it does', async () => {
    let releaseDetail = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseDetail = () => { resolve(); }; });
    const { sent } = harness({ heldDetail: held });

    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));

    // The reader goes at once — nothing on this page is waiting for the read.
    await waitFor(() => { expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w-new`); });
    expect(createdWaveBodies(sent)).toHaveLength(1);
    await findWavePage();
    expect(screen.queryByRole('complementary', { name: 'Spec' })).toBeNull();

    releaseDetail();
    await held;
    expect(await screen.findByRole('complementary', { name: 'Spec' })).toBeTruthy();
  }, 10_000);

  /*
   * A create that lands after the reader has moved on must not yank them back.
   *
   * `POST /api/waves` can be slow, and nothing stops them pressing Back or
   * picking a rail row while it is in flight. The route unmounts but the
   * promise continuation still runs, and an unguarded `go()` pulled them off
   * the page they had just chosen. The wave is created either way and is in the
   * rail; being navigated costs them their own last action.
   */
  it('does not yank the reader back when the create lands after they left', async () => {
    let releaseCreate = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseCreate = () => { resolve(); }; });
    const { sent } = harness({ heldCreate: held });

    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));

    // They leave while the create is still in flight.
    await userEvent.click(await screen.findByRole('button', { name: 'Today' }));
    await waitFor(() => { expect(window.location.pathname).toBe(`${APP_BASEPATH}/`); });

    releaseCreate();
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    // Still where they chose to be.
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/`);

    /*
     * And nothing was written into the registry on the way past.
     *
     * Asserting only the pathname above is not enough, and that gap is exactly
     * what review caught by execution on the shape this replaced: a drawer
     * request written into a provider after the reader left was invisible
     * *here* and surfaced on their **next** visit to the wave as a Spec drawer
     * nobody opened. The intent now rides on the history entry the create would
     * have made — and it never made one — so the later visit is the observable
     * that says so, and it stays the observable regardless of how the intent is
     * carried.
     */
    window.history.pushState({}, '', `${APP_BASEPATH}/wave/w-new`);
    /* Wait for the page to be far enough along that a leftover request *would*
       have been redeemed — the title only renders once the wave detail has
       landed, which is the same read the drawer needs. Asserting absence before
       that is asserting that nothing has happened yet, which is true either
       way; the first two attempts at this test both failed that way. */
    await screen.findByRole('button', { name: 'Rename wave' });
    expect(screen.queryByRole('complementary', { name: 'Spec' })).toBeNull();
  }, 10_000);

  /* A wave detail that will not load costs a closed drawer and nothing else, so
     it must not block the navigation — there is no message riding on it. */
  it('still lands on the wave when the wave detail read fails', async () => {
    const { sent } = harness({
      waveDetail: { status: 500, statusText: 'Server Error', body: { error: 'boom' } },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));

    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/wave/w-new`));
  });

  it('reports a create that failed, and creates nothing', async () => {
    const { sent } = harness({
      waveCreate: { status: 500, statusText: 'Server Error', body: { error: 'boom' } },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));

    expect(await screen.findByRole('alert')).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/cove/c2/new`);
    expect(composerText()).toBe('Read it');
    expect(specInputTexts(sent)).toEqual([]);
  });
});
