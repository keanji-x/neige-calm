// @vitest-environment jsdom
// The shell's New wave dialog: one dialog, two entry points. `cove_id` is the
// opener's cove; the folder is optional and decides the whole request shape —
// no folder omits `cwd` *and* `attach_folder` (the kernel's managed default),
// a chosen folder sends both (#1147 S3).
//
// This drives the real router, the real QueryClient and the real form — the
// wiring *is* the thing under test, and a fixture that re-implemented the
// branch would prove only that the fixture agrees with itself.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
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

const LISTING = {
  path: '/srv/app', parent: '/srv', entries: [{ name: 'crates', is_dir: true }],
};

/** The 409 `POST /api/waves` answers a folder clash with — no `error` key. */
const CONFLICT = {
  folder_id: 4, cove_id: 'c1', conflict_path: '/srv/app', conflict_kind: 'descendant',
};

function harness(options: { waveCreate?: ApiTransportResponse } = {}) {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      const posted = request.body as { cove_id?: string } | undefined;
      if (request.method === 'POST' && request.path === '/api/waves' && options.waveCreate) {
        return Promise.resolve(options.waveCreate);
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

  it('posts the opener\'s cove_id and omits cwd / attach_folder with no folder chosen', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    // #1161 — establish the dialog is open *and exposed* first. The
    // `queryByLabelText` absence check below would pass vacuously against a
    // dialog that never opened, and `getByLabelText` does no accessibility
    // filtering, so it cannot stand in for this wait.
    expect(await screen.findByRole('dialog', { name: 'New wave' })).toBeTruthy();
    expect(screen.queryByLabelText('Cove')).toBeNull();
    await userEvent.type(await screen.findByLabelText('Task'), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ cove_id: 'c2', title: 'Read it' });
    expect(body).toHaveProperty('theme');
    // The managed-workspace branch is keyed on *absence*, not on a value:
    // `cwd: null` and `attach_folder: false` are both a different kernel path.
    expect(body).not.toHaveProperty('cwd');
    expect(body).not.toHaveProperty('attach_folder');
    expect(sent.some((request) => request.path.startsWith('/api/fs/listdir'))).toBe(false);
  });

  /*
   * The other half of the same contract. `attach_folder: true` is not decorative
   * — with it omitted the kernel refuses any path no cove has already claimed,
   * so an attached create would 409 for exactly the folders a user is most
   * likely to pick. It is a no-op when this cove already covers the path.
   */
  it('posts the picked folder as cwd with attach_folder: true', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New wave' }));
    expect(await screen.findByRole('dialog', { name: 'New wave' })).toBeTruthy();
    await userEvent.type(await screen.findByLabelText('Task'), 'Read it');

    await userEvent.click(await screen.findByLabelText('Folder'));
    // The picker pushes into the *same* dialog rather than opening a second
    // one — the frozen `DirectoryField` contract, and the reason this assertion
    // is on the dialog's accessible name and not on a second dialog node.
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    expect(await screen.findByRole('dialog', { name: 'New wave' })).toBeTruthy();

    await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    const body = createdWaveBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({
      cove_id: 'c1', title: 'Read it', cwd: '/srv/app', attach_folder: true,
    });
    expect(sent.some((request) => request.path === '/api/fs/listdir')).toBe(true);
  });

  /*
   * The 409 body has no `error` key, so `ApiError.message` is the bare status
   * text: without decoding it the user is told "Conflict" and nothing else —
   * not which folder, not which cove, not what to do instead.
   */
  it('renders the structured folder conflict, not the word Conflict', async () => {
    harness({ waveCreate: { status: 409, statusText: 'Conflict', body: CONFLICT } });
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    expect(await screen.findByRole('dialog', { name: 'New wave' })).toBeTruthy();
    await userEvent.type(await screen.findByLabelText('Task'), 'Read it');
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
});
