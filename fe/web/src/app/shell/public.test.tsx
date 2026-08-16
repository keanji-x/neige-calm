// @vitest-environment jsdom
// The shell's New wave dialog: one dialog, two entry points, and a body whose
// `cwd` / `attach_folder` come from the target cove's claimed folders.
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
import { createAppRouter } from '../router/public.tsx';
import { ThemeProvider } from '../theme/public.tsx';

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

function folderRow(id: number, coveId: string, path: string) {
  return { id, cove_id: coveId, path, repo_identity: null, repo_identity_probed_at: null, created_at: 1 };
}

/** Folders per cove: `c1` owns one, `c2` owns none — the two branches. */
const FOLDERS: Record<string, ReturnType<typeof folderRow>[]> = {
  c1: [folderRow(1, 'c1', '/srv/work')],
  c2: [],
};

function harness() {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      const folders = /^\/api\/coves\/([^/]+)\/folders$/.exec(request.path);
      const body = request.path === '/api/coves' ? [COVE, OTHER]
        : folders ? FOLDERS[folders[1] ?? ''] ?? []
          : request.method === 'POST' && request.path === '/api/waves'
            ? { ...COVE, id: 'w-new', cove_id: 'c1', title: 'x', sort: 0 }
            : [];
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  window.history.pushState({}, '', '/cove/c1');
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, onSignOut: vi.fn() });
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
    expect(screen.getByRole('dialog', { name: 'New wave' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());

    // The cove page's WAVES module head opens *the same* dialog — one title,
    // one Cove select, one set of strings.
    await userEvent.click(screen.getByRole('button', { name: 'New wave' }));
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    expect(screen.getByRole('dialog', { name: 'New wave' })).toBeTruthy();
  });

  it('preselects the cove the entry point named', async () => {
    harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    expect(screen.getByLabelText('Cove')).toHaveProperty('value', 'c2');
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));

    await userEvent.click(screen.getByRole('button', { name: 'New wave in Work' }));
    expect(screen.getByLabelText('Cove')).toHaveProperty('value', 'c1');
  });
});

describe('the dialog derives cwd and attach_folder from the cove\'s folders', () => {
  it('sends the claimed folder and claims nothing when the cove already has one', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Work' }));
    await waitFor(() => expect(sent.some((request) => request.path === '/api/coves/c1/folders')).toBe(true));
    // No path field at all: the cove owns exactly one folder, so there is
    // nothing to ask and nothing for the user to retype.
    await waitFor(() => expect(screen.queryByLabelText('Folder')).toBeNull());
    await userEvent.type(screen.getByLabelText('Task'), 'Ship it');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({
      cove_id: 'c1', title: 'Ship it', cwd: '/srv/work', attach_folder: false,
    });
  });

  it('asks for a first folder and claims it when the cove has none', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Reading' }));
    const field = await screen.findByLabelText('Folder');
    await userEvent.type(screen.getByLabelText('Task'), 'Read it');
    await userEvent.type(field, '/home/reading');
    await userEvent.click(screen.getByRole('button', { name: 'Create wave' }));
    await waitFor(() => expect(createdWaveBodies(sent)).toHaveLength(1));
    expect(createdWaveBodies(sent)[0]).toMatchObject({
      cove_id: 'c2', title: 'Read it', cwd: '/home/reading', attach_folder: true,
    });
  });

  /* Changing the cove in the dialog re-reads folders, so the form's shape
     follows the select rather than the entry point that opened it. */
  it('re-evaluates the branch when the user changes cove mid-form', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New wave in Work' }));
    await waitFor(() => expect(sent.some((request) => request.path === '/api/coves/c1/folders')).toBe(true));
    expect(screen.queryByLabelText('Folder')).toBeNull();

    await userEvent.selectOptions(screen.getByLabelText('Cove'), 'c2');
    expect(await screen.findByLabelText('Folder')).toBeTruthy();
  });
});
