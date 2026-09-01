// @vitest-environment jsdom
// The shell's New track dialog: one dialog, two entry points. `cove_id` is the
// opener's cove; the folder is optional and decides the whole request shape —
// no folder omits `cwd` *and* `attach_folder` (the kernel's managed default),
// a chosen folder sends both (#1147 S3).
//
// This drives the real router, the real QueryClient and the real form — the
// wiring *is* the thing under test, and a fixture that re-implemented the
// branch would prove only that the fixture agrees with itself.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { APP_BASEPATH, createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
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

/* The Task field's accessible name after #1209's astryx rewrite: the label is
   visually hidden (the field is one line and the placeholder already says what
   it wants), so the name is spelled out here on purpose — losing it would make
   the field unreachable by screen reader and by voice control. */
const TASK_LABEL = 'What this track should accomplish';

const COVE = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const OTHER = { id: 'c2', name: 'Reading', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };

const LISTING = {
  path: '/srv/app', parent: '/srv', entries: [{ name: 'crates', is_dir: true }],
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

function harness(options: { templates?: unknown; waveCreate?: ApiTransportResponse } = {}) {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      const posted = request.body as { cove_id?: string } | undefined;
      if (request.method === 'POST' && request.path === '/api/waves' && options.waveCreate) {
        return Promise.resolve(options.waveCreate);
      }
      if (request.path === '/api/wave-templates') {
        // `undefined` here is the read failing outright — the branch the
        // dialog must survive.
        const templates = options.templates;
        return templates === undefined
          ? Promise.resolve({ status: 500, statusText: 'Server Error', body: { message: 'boom' } })
          : Promise.resolve({ status: 200, statusText: 'OK', body: templates });
      }
      const body = request.path === '/api/coves' ? [COVE, OTHER]
        : request.path.startsWith('/api/fs/listdir') ? LISTING
          : request.method === 'POST' && request.path === '/api/waves'
            ? { ...COVE, id: 'w-new', cove_id: posted?.cove_id ?? 'c1', title: 'x', sort: 0 }
            : [];
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  window.history.pushState({}, '', `${APP_BASEPATH}/cove/c1`);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
  });
  render(
    <QueryClientProvider client={client}>
      <ThemeProvider storage={memoryStorage()}>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return { sent };
}

function createdWaveBodies(sent: readonly ApiRequest[]): unknown[] {
  return sent.filter((request) => request.method === 'POST' && request.path === '/api/waves')
    .map((request) => request.body);
}

describe('the New track dialog is the shell\'s, and both entry points open it', () => {
  it('opens the same dialog from the rail and from the cove page', async () => {
    harness();
    // The rail's `+`, on a cove the user is not currently inside: the whole
    // point of the row control is starting a wave without navigating first.
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    // #1161 — every role query below depends on the dialog's *open
    // accessibility state*, which no click can promise synchronously. Wait for
    // it; do not assume the click already published it.
    expect(await screen.findByRole('dialog', { name: 'New track' })).toBeTruthy();
    await userEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());

    // The cove page's WAVES module head opens *the same* dialog — one title,
    // one Task field, one set of strings.
    // Closing puts the rail and the page back in the accessibility tree by
    // effect cleanup, i.e. not necessarily by the time the click above
    // resolves — so this opener is a `findBy` too.
    await userEvent.click(await screen.findByRole('button', { name: 'New track' }));
    expect(await screen.findByRole('dialog', { name: 'New track' })).toBeTruthy();
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
  });

  /*
   * #1161. The dialog's opening focus went to `focusables(panel)[0]`, which is
   * the header's Close button, so a reader who opened this and typed put
   * nothing in the field — and **space activates a focused button**, so the
   * first space in a title threw the dialog away. It read as a flaky test
   * because whether the frame landed before or after the reader's click
   * decided which of the two failures happened.
   *
   * Asserted through the shell rather than against `<Dialog>` directly,
   * because the defect was a missing `initialFocusRef` at this call site while
   * the primitive was working as designed.
   */
  it('opens with the Task field focused, so typing a title reaches it', async () => {
    harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await screen.findByRole('dialog', { name: 'New track' });
    await act(async () => { await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); }); });

    expect(document.activeElement).toBe(screen.getByLabelText(TASK_LABEL));

    // The consequence, stated as behaviour: the space in "Read it" is what used
    // to close the dialog.
    await userEvent.keyboard('Read it');
    expect(screen.getByLabelText<HTMLInputElement>(TASK_LABEL).value).toBe('Read it');
    expect(screen.getByRole('dialog', { name: 'New track' })).toBeTruthy();
  });

  it('posts the opener\'s cove_id and omits cwd / attach_folder with no folder chosen', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    // #1161 — establish the dialog is open *and exposed* first. The
    // `queryByLabelText` absence check below would pass vacuously against a
    // dialog that never opened, and `getByLabelText` does no accessibility
    // filtering, so it cannot stand in for this wait.
    expect(await screen.findByRole('dialog', { name: 'New track' })).toBeTruthy();
    expect(screen.queryByLabelText('Cove')).toBeNull();
    /* #1147 S3 restated on top of #1209: the Folder control *is* here — this
       assertion used to be `toBeNull()` — and it starts empty. Empty is what
       "no folder chosen" looks like, and it is what the absence checks on the
       body below are the consequence of. */
    expect(screen.getByLabelText('Folder').textContent).toContain('Neige picks a workspace for this track');
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ cove_id: 'c2', title: 'Read it' });
    expect(body).toHaveProperty('theme');
    // The managed-workspace branch is keyed on *absence*, not on a value:
    // `cwd: null` and `attach_folder: false` are both a different kernel path.
    expect(body).not.toHaveProperty('cwd');
    expect(body).not.toHaveProperty('attach_folder');
    expect(sent.some((request) => request.path.startsWith('/api/fs/listdir'))).toBe(false);
    // #1209 — Blank is the default, and Blank means the key is not on the wire
    // at all. `workflow_id: null` or `''` is a 400 from the kernel.
    expect(body).not.toHaveProperty('workflow_id');
    expect(body).not.toHaveProperty('workflow_input');
  });

  /*
   * The other half of the same contract. `attach_folder: true` is not decorative
   * — with it omitted the kernel refuses any path no cove has already claimed,
   * so an attached create would 409 for exactly the folders a user is most
   * likely to pick. It is a no-op when this cove already covers the path.
   */
  it('posts the picked folder as cwd with attach_folder: true', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track' }));
    expect(await screen.findByRole('dialog', { name: 'New track' })).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');

    await userEvent.click(await screen.findByLabelText('Folder'));
    // The picker pushes into the *same* dialog rather than opening a second
    // one — the frozen `DirectoryField` contract, and the reason this assertion
    // is on the dialog's accessible name and not on a second dialog node.
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    expect(await screen.findByRole('dialog', { name: 'New track' })).toBeTruthy();

    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({
      cove_id: 'c1', title: 'Read it', cwd: '/srv/app', attach_folder: true,
    });
    expect(sent.some((request) => request.path === '/api/fs/listdir')).toBe(true);
    // Attaching a folder is orthogonal to #1209's template choice: staying on
    // Blank must still keep `workflow_id` off the wire.
    expect(body).not.toHaveProperty('workflow_id');
  });

  /*
   * The two features on one request. The folder and the template are collected
   * by different controls and translated by different branches of
   * `submitNewWave`, and nothing else proves the second spread does not
   * clobber the first.
   */
  it('carries a chosen folder and a chosen template on the same POST', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track' }));
    expect(await screen.findByRole('dialog', { name: 'New track' })).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: /^Start from/ }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));

    await userEvent.click(await screen.findByLabelText('Folder'));
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({
      cove_id: 'c1',
      title: 'Read it',
      workflow_id: 'small-change',
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
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    expect(await screen.findByRole('dialog', { name: 'New track' })).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    // The request, its rejection, and the re-render are three ticks the click
    // does not await; the default 1s window is not enough under a loaded suite.
    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    expect(alert.textContent).toContain('/srv/app');
    // `c1` is Work in the seeded cove list — the id must never reach the page.
    expect(alert.textContent).toContain('area “Work”');
    expect(alert.textContent).not.toContain('c1');
    expect(alert.textContent).not.toBe('Conflict');
  });

  /*
   * #1209, through the shell rather than the form: the form builds a draft,
   * but only this wiring decides what reaches the wire. A form-level test
   * cannot see `submitNewWave` dropping a field on the way to the POST.
   */
  it('carries the chosen template onto the create POST', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await screen.findByRole('dialog', { name: 'New track' });
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Fix the thing');
    /* `findBy`: the picker's trigger is there from the first paint, but the
       option only exists once the template read has landed — so the wait is on
       the option inside the opened menu, not on the trigger. */
    await userEvent.click(screen.getByRole('button', { name: /^Start from/ }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Issue development/ }));
    await userEvent.type(
      screen.getByLabelText('Issue URL'),
      'https://github.com/keanji-x/neige-calm/issues/1209',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({
      cove_id: 'c2',
      title: 'Fix the thing',
      workflow_id: 'issue-development',
      workflow_input: {
        issue_url: 'https://github.com/keanji-x/neige-calm/issues/1209',
        repo: 'keanji-x/neige-calm',
        issue_number: 1209,
        merge_policy: 'hold-for-ratify',
      },
    });
  });

  it('sends an unbound template as an id with no workflow_input', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await screen.findByRole('dialog', { name: 'New track' });
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Tiny fix');
    await userEvent.click(screen.getByRole('button', { name: /^Start from/ }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ title: 'Tiny fix', workflow_id: 'small-change' });
    expect(body).not.toHaveProperty('workflow_input');
  });

  /*
   * The real failure mode, driven end to end: the template read 500s and the
   * app's only wave-creation entry point still creates a wave. Asserted here
   * and not only on the form because the degradation lives in the wiring —
   * `data ?? []` plus a query that does not retry.
   */
  it('still creates a wave when the template read fails outright', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await screen.findByRole('dialog', { name: 'New track' });
    /* Wait for the failure to *land*, not for the request to leave. Waiting on
       `sent` only proves the query started: react-query could still be pending
       when the submit runs, and then this case would silently be testing
       "submits while the list is loading" — a different, easier branch.
       The rendered notice is the first observable moment the 500 has been
       consumed (`useWaveTemplates` turns `isError` into this string), so it is
       what the wait is on. */
    await screen.findByText(/Could not load templates/);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it anyway');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({ cove_id: 'c2', title: 'Read it anyway' });
  });
});
