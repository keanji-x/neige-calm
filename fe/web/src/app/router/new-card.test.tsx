// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { CardWire } from '../../../../core/domain/track.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = {
  id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

function setup({ createFails = false, deferCreate = false } = {}) {
  const requests: ApiRequest[] = [];
  const cards: CardWire[] = [];
  /* One entry per POST, in send order, so a test can release an older attempt before a newer one. */
  const releases: (() => void)[] = [];
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({ track: TRACK, can_resume: false, cards: [...cards], overlays: [] }));
      }
      if (request.path === '/api/tracks/w1/report') return Promise.resolve(ok({ taskDiagnostics: [] }));
      if (request.path.startsWith('/api/fs/listdir')) {
        return Promise.resolve(ok({
          path: '/repo', parent: '/', entries: [{ name: 'notes.md', is_dir: false }],
        }));
      }
      if (request.method === 'POST' && request.path.startsWith('/api/tracks/w1/')) {
        if (createFails) {
          return Promise.resolve({
            status: 500, statusText: 'Server Error', body: { error: 'the kernel refused this card' },
          });
        }
        const created: CardWire = {
          id: `card-${cards.length + 1}`, track_id: 'w1', kind: 'terminal', title: null, sort: 1,
          payload: {}, deletable: true, created_at: 1, updated_at: 2,
        };
        cards.push(created);
        if (!deferCreate) return Promise.resolve(ok(created));
        return new Promise<ApiTransportResponse>((resolve) => {
          releases.push(() => { resolve(ok(created)); });
        });
      }
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
  });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return {
    requests, router, releases,
    posts: () => requests.filter((request) => request.method === 'POST'),
  };
}

async function pickKind(label: string) {
  const trigger = await screen.findByRole('button', { name: 'Add card' });
  /* The popover's close from the previous cycle can land after the next click and
       read as a close, so the click is retried until the menu is actually showing. */
  await waitFor(async () => {
    if (screen.queryByRole('menuitem', { name: label }) !== null) return;
    await userEvent.click(trigger);
    expect(screen.queryByRole('menuitem', { name: label })).not.toBeNull();
  });
  await userEvent.click(screen.getByRole('menuitem', { name: label }));
}

beforeEach(() => {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
  Element.prototype.scrollIntoView = vi.fn();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('adding a card from the CARDS module', () => {
  it('says so on screen when a fieldless kind fails to create', async () => {
    setup({ createFails: true });
    await pickKind('terminal');
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('the kernel refused this card');
    // No dialog was opened for this kind, so the message cannot have come from `NewCardForm`.
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('shows a failed create inside the dialog exactly once for a kind with fields', async () => {
    setup({ createFails: true });
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(document.querySelectorAll('[data-nc-new-card-error]')).toHaveLength(1); });
    expect(document.querySelectorAll('[data-nc-error-box]')).toHaveLength(0);
  });

  it('sends a codex card to the atomic codex endpoint', async () => {
    const { posts } = setup();
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(posts()).toHaveLength(1); });
    const [post] = posts();
    expect(post?.path).toBe('/api/tracks/w1/codex-cards');
    // `theme` is required by the kernel (422 without it): the daemon answers
    // codex's OSC 10/11 probe with these colours.
    expect(post?.body).toHaveProperty('theme');
  });

  it('does not navigate when a create lands after the reader left the track', async () => {
    const { router, releases, posts } = setup({ deferCreate: true });
    await pickKind('terminal');
    await waitFor(() => { expect(posts()).toHaveLength(1); });
    expect(releases).toHaveLength(1);

    await act(async () => { await router.navigate({ to: '/' }); });
    await waitFor(() => { expect(router.state.location.pathname).toBe('/'); });

    await act(async () => {
      releases[0]?.();
      await new Promise((done) => { setTimeout(done, 0); });
    });

    expect(router.state.location.pathname).toBe('/');
    expect(router.state.location.searchStr).not.toContain('card=');
  });

  it('keeps reporting the create in flight when a superseded attempt lands', async () => {
    const { router, releases, posts } = setup({ deferCreate: true });
    await pickKind('terminal');
    await waitFor(() => { expect(posts()).toHaveLength(1); });
    await pickKind('terminal');
    await waitFor(() => { expect(posts()).toHaveLength(2); });

    // Open a kind with fields purely to read the busy state off its submit.
    await pickKind('codex');
    expect(await screen.findByRole('button', { name: 'Creating…' })).toBeTruthy();

    await act(async () => {
      releases[0]?.();
      await new Promise((done) => { setTimeout(done, 0); });
    });

    expect(screen.getByRole('button', { name: 'Creating…' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Create codex' })).toBeNull();
    expect(router.state.location.searchStr).not.toContain('card=');
  });

  it('sends a file card to the generic create with the entry kind and payload', async () => {
    const { posts } = setup();
    await pickKind('file');
    await userEvent.click(await screen.findByRole('button', { name: 'File or folder' }));
    await userEvent.click(await screen.findByRole('option', { name: 'notes.md' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Create file' }));
    await waitFor(() => { expect(posts()).toHaveLength(1); });
    const [post] = posts();
    expect(post?.path).toBe('/api/tracks/w1/cards');
    expect(post?.body).toMatchObject({ kind: 'file-viewer', payload: { path: '/repo/notes.md' } });
  });
});
