// @vitest-environment jsdom
// The shell's New wave dialog: one dialog, two entry points, title only.
// `cove_id` is the opener's cove; the POST omits `cwd` / `attach_folder`.
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

const COVE = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const OTHER = { id: 'c2', name: 'Reading', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };

/* #1209 — what `GET /api/wave-templates` returns, in the two shapes that
   matter: one template bound to a running plugin (an `input_schema`, therefore
   fields) and one that is not. */
const TEMPLATES = [
  { id: 'small-change', title: 'Small change' },
  {
    id: 'issue-development',
    title: 'Issue development',
    input_schema: { type: 'object', required: ['issue_url', 'repo', 'issue_number'] },
  },
];

function harness(options: { templates?: unknown } = {}) {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      const posted = request.body as { cove_id?: string } | undefined;
      if (request.path === '/api/wave-templates') {
        // `undefined` here is the read failing outright — the branch the
        // dialog must survive.
        const templates = options.templates;
        return templates === undefined
          ? Promise.resolve({ status: 500, statusText: 'Server Error', body: { message: 'boom' } })
          : Promise.resolve({ status: 200, statusText: 'OK', body: templates });
      }
      const body = request.path === '/api/coves' ? [COVE, OTHER]
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

describe('the New wave dialog is the shell\'s, and both entry points open it', () => {
  it('opens the same dialog from the rail and from the cove page', async () => {
    harness();
    // The rail's `+`, on a cove the user is not currently inside: the whole
    // point of the row control is starting a wave without navigating first.
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    // #1161 — every role query below depends on the dialog's *open
    // accessibility state*, which no click can promise synchronously. Wait for
    // it; do not assume the click already published it.
    expect(await screen.findByRole('dialog', { name: 'New wave' })).toBeTruthy();
    await userEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());

    // The cove page's WAVES module head opens *the same* dialog — one title,
    // one Task field, one set of strings.
    // Closing puts the rail and the page back in the accessibility tree by
    // effect cleanup, i.e. not necessarily by the time the click above
    // resolves — so this opener is a `findBy` too.
    await userEvent.click(await screen.findByRole('button', { name: 'New wave' }));
    expect(await screen.findByRole('dialog', { name: 'New wave' })).toBeTruthy();
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
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await screen.findByRole('dialog', { name: 'New wave' });
    await act(async () => { await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); }); });

    expect(document.activeElement).toBe(screen.getByLabelText('Task'));

    // The consequence, stated as behaviour: the space in "Read it" is what used
    // to close the dialog.
    await userEvent.keyboard('Read it');
    expect(screen.getByLabelText<HTMLInputElement>('Task').value).toBe('Read it');
    expect(screen.getByRole('dialog', { name: 'New wave' })).toBeTruthy();
  });

  it('posts the opener\'s cove_id and omits cwd / attach_folder', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    // #1161 — establish the dialog is open *and exposed* first. The two
    // `queryByLabelText` absence checks below would pass vacuously against a
    // dialog that never opened, and `getByLabelText` does no accessibility
    // filtering, so neither of them can stand in for this wait.
    expect(await screen.findByRole('dialog', { name: 'New wave' })).toBeTruthy();
    expect(screen.queryByLabelText('Cove')).toBeNull();
    expect(screen.queryByLabelText('Folder')).toBeNull();
    await userEvent.type(screen.getByLabelText('Task'), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ cove_id: 'c2', title: 'Read it' });
    expect(body).toHaveProperty('theme');
    expect(body).not.toHaveProperty('cwd');
    expect(body).not.toHaveProperty('attach_folder');
    // #1209 — Blank is the default, and Blank means the key is not on the wire
    // at all. `workflow_id: null` or `''` is a 400 from the kernel.
    expect(body).not.toHaveProperty('workflow_id');
    expect(body).not.toHaveProperty('workflow_input');
  });

  /*
   * #1209, through the shell rather than the form: the form builds a draft,
   * but only this wiring decides what reaches the wire. A form-level test
   * cannot see `submitNewWave` dropping a field on the way to the POST.
   */
  it('carries the chosen template onto the create POST', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await screen.findByRole('dialog', { name: 'New wave' });
    await userEvent.type(screen.getByLabelText('Task'), 'Fix the thing');
    // `findBy`: the row only exists once the template read has landed.
    await userEvent.click(await screen.findByRole('radio', { name: 'Issue development' }));
    await userEvent.type(
      screen.getByLabelText('Issue URL'),
      'https://github.com/keanji-x/neige-calm/issues/1209',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));
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
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await screen.findByRole('dialog', { name: 'New wave' });
    await userEvent.type(screen.getByLabelText('Task'), 'Tiny fix');
    await userEvent.click(await screen.findByRole('radio', { name: 'Small change' }));
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));
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
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    await screen.findByRole('dialog', { name: 'New wave' });
    await waitFor(() => expect(sent.some((r) => r.path === '/api/wave-templates')).toBe(true));
    await userEvent.type(screen.getByLabelText('Task'), 'Read it anyway');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({ cove_id: 'c2', title: 'Read it anyway' });
  });
});
