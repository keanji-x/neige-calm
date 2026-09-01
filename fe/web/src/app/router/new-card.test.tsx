// @vitest-environment jsdom
//
// Adding a card, driven through the real route: the real registry, the real
// built-in entries, the real `AddCardMenu` and the real mutations. Two things
// are under test and neither can be seen from a unit:
//
//  1. **A failed create is rendered.** A kind with no fields never opens the
//     dialog, so a message only `NewCardForm` can draw is a message that path
//     never shows — the `+` menu closed and nothing at all happened.
//  2. **Which endpoint a kind takes.** `createCardOfKind` is the one place that
//     decides between the atomic per-kind endpoints and the generic
//     `POST /api/waves/:id/cards`, and the only evidence is the request.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { CardWire } from '../../../../core/domain/wave.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const COVE = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = {
  id: 'w1', cove_id: 'c1', title: 'Test wave', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

function setup({ createFails = false, deferCreate = false } = {}) {
  const requests: ApiRequest[] = [];
  const cards: CardWire[] = [];
  /* Held open so the reader can leave the wave *while* the create is in
     flight — the window the route has to survive. */
  const release: { current: (() => void) | null } = { current: null };
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      if (request.path === '/api/coves') return Promise.resolve(ok([COVE]));
      if (request.path === '/api/coves/c1/waves') return Promise.resolve(ok([WAVE]));
      if (request.path === '/api/waves/w1') {
        return Promise.resolve(ok({ wave: WAVE, cards: [...cards], overlays: [] }));
      }
      if (request.path === '/api/waves/w1/report') return Promise.resolve(ok({ taskDiagnostics: [] }));
      if (request.path.startsWith('/api/fs/listdir')) {
        return Promise.resolve(ok({
          path: '/repo', parent: '/', entries: [{ name: 'notes.md', is_dir: false }],
        }));
      }
      if (request.method === 'POST' && request.path.startsWith('/api/waves/w1/')) {
        if (createFails) {
          return Promise.resolve({
            status: 500, statusText: 'Server Error', body: { error: 'the kernel refused this card' },
          });
        }
        const created: CardWire = {
          id: `card-${cards.length + 1}`, wave_id: 'w1', kind: 'terminal', title: null, sort: 1,
          payload: {}, deletable: true, created_at: 1, updated_at: 2,
        };
        cards.push(created);
        if (!deferCreate) return Promise.resolve(ok(created));
        return new Promise<ApiTransportResponse>((resolve) => {
          release.current = () => { resolve(ok(created)); };
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
  router.update({ history: createMemoryHistory({ initialEntries: ['/wave/w1'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return {
    requests, router, release,
    posts: () => requests.filter((request) => request.method === 'POST'),
  };
}

async function pickKind(label: string) {
  await userEvent.click(await screen.findByRole('button', { name: 'Add card' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: label }));
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
  /*
   * The fieldless kind is the one with no dialog to carry the sentence, which is
   * why the route needs a surface of its own. Before it had one this gesture
   * ended in silence: the menu closed, no card appeared, and nothing said why.
   */
  it('says so on screen when a fieldless kind fails to create', async () => {
    setup({ createFails: true });
    await pickKind('terminal');
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('the kernel refused this card');
    // No dialog was ever opened for this kind, so the message cannot have come
    // from `NewCardForm`.
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  /* The dialog keeps showing it inline, and the route does not print it a
     second time behind the open dialog. */
  it('shows a failed create inside the dialog exactly once for a kind with fields', async () => {
    setup({ createFails: true });
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(document.querySelectorAll('[data-nc-new-card-error]')).toHaveLength(1); });
    expect(document.querySelectorAll('[data-nc-error-box]')).toHaveLength(0);
  });

  /*
   * ── Which door each kind takes ────────────────────────────────────────────
   *
   * `codex` owns a runtime, so the kernel writes the row and spawns the daemon
   * in one call on an endpoint named after the kind. `file-viewer` owns none, so
   * the row is all there is and it goes through the generic create with the
   * entry's own `claim.kind` and `buildPayload`.
   */
  it('sends a codex card to the atomic codex endpoint', async () => {
    const { posts } = setup();
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(posts()).toHaveLength(1); });
    const [post] = posts();
    expect(post?.path).toBe('/api/waves/w1/codex-cards');
    // `theme` is required by the kernel (422 without it): the daemon answers
    // codex's OSC 10/11 probe with these colours.
    expect(post?.body).toHaveProperty('theme');
  });

  /*
   * A create is a write plus a navigation, and only the write belongs to the
   * kernel. If the reader leaves the wave while the post is in flight, landing
   * the answer still steered them back to the wave they had just left — a
   * navigation nobody asked for, on top of state written into an unmounted
   * body. The abort is per attempt and fires on unmount, exactly as the delete
   * path's does; the card itself is still created server-side, which is why the
   * assertion is about where the reader is, not about the request.
   */
  it('does not navigate when a create lands after the reader left the wave', async () => {
    const { router, release, posts } = setup({ deferCreate: true });
    await pickKind('terminal');
    await waitFor(() => { expect(posts()).toHaveLength(1); });
    expect(release.current).toBeTypeOf('function');

    // The reader moves on while the create is still in flight: this unmounts
    // the wave route body that owns the pending create.
    await act(async () => { await router.navigate({ to: '/' }); });
    await waitFor(() => { expect(router.state.location.pathname).toBe('/'); });

    await act(async () => {
      release.current?.();
      await new Promise((done) => { setTimeout(done, 0); });
    });

    expect(router.state.location.pathname).toBe('/');
    expect(router.state.location.searchStr).not.toContain('card=');
  });

  it('sends a file card to the generic create with the entry kind and payload', async () => {
    const { posts } = setup();
    await pickKind('file');
    // The path is picked, not typed: the field is a browser over the real
    // listing operation, which is the only way this kind gets a payload.
    await userEvent.click(await screen.findByRole('button', { name: 'File or folder' }));
    await userEvent.click(await screen.findByRole('option', { name: 'notes.md' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Create file' }));
    await waitFor(() => { expect(posts()).toHaveLength(1); });
    const [post] = posts();
    expect(post?.path).toBe('/api/waves/w1/cards');
    expect(post?.body).toMatchObject({ kind: 'file-viewer', payload: { path: '/repo/notes.md' } });
  });
});
