// @vitest-environment jsdom

import { onlineManager, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
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
  id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, cwd: '/tmp',
  pinned_at: null, closed_at: null, created_at: 1, updated_at: 2,
};

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

/** How the fake server answers one create, by its 0-based place among the creates: `created` mints a card. */
type CreateAnswer = 'created' | 'lost' | ApiTransportResponse;

function setup({ createFails = false, deferCreate = false, answers = [] as readonly CreateAnswer[] } = {}) {
  const requests: ApiRequest[] = [];
  const cards: CardWire[] = [];
  const byKey = new Map<string, { body: string; card: CardWire }>();
  /* One entry per POST, in send order, so a test can release an older attempt before a newer one. */
  const releases: (() => void)[] = [];
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({ track: TRACK, can_reopen: false, can_close: true, cards: [...cards], overlays: [] }));
      }
      if (request.path === '/api/tracks/w1/report') return Promise.resolve(ok({ taskDiagnostics: [] }));
      if (request.path.startsWith('/api/fs/listdir')) {
        return Promise.resolve(ok({
          path: '/repo', parent: '/', entries: [{ name: 'notes.md', is_dir: false }],
        }));
      }
      if (request.method === 'POST' && request.path.startsWith('/api/tracks/w1/')) {
        const answer = answers[requests.filter((sent) => sent.method === 'POST').length - 1] ?? 'created';
        /* Answered before anything was made. */
        if (answer !== 'created' && answer !== 'lost') return Promise.resolve(answer);
        if (createFails) {
          return Promise.resolve({
            status: 500, statusText: 'Server Error', body: { error: 'the kernel refused this card' },
          });
        }
        /* The kernel's keyed create: a retry under a stored key joins its card, a different body under it is refused. */
        const key = request.headers?.['Idempotency-Key'];
        const stored = key === undefined ? undefined : byKey.get(key);
        if (stored !== undefined && stored.body !== JSON.stringify(request.body)) {
          return Promise.resolve({ status: 409, statusText: 'Conflict', body: {
            error: 'This card request was already used for a different card.', code: 'idempotency_key_reused',
          } });
        }
        const created: CardWire = stored?.card ?? {
          id: `card-${cards.length + 1}`, track_id: 'w1', kind: 'terminal', title: null, sort: 1,
          payload: {}, deletable: true, created_at: 1, updated_at: 2,
        };
        if (stored === undefined) {
          cards.push(created);
          if (key !== undefined) byKey.set(key, { body: JSON.stringify(request.body), card: created });
        }
        /* Made, but the answer never arrived. */
        if (answer === 'lost') return Promise.reject(new Error('socket hang up'));
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
    requests, router, releases, cards,
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
  onlineManager.setOnline(true);
});

describe('adding a card from the CARDS module', () => {
  it('says so on screen when a fieldless kind fails to create', async () => {
    setup({ createFails: true });
    await pickKind('terminal');
    const alert = await screen.findByRole('alert');
    /* A 500 may follow a card that was made: the fixed unknown state, not the server's words (#2131). */
    expect(within(alert).getByText('Creating the terminal card is unconfirmed.')).toBeTruthy();
    expect(within(alert).getByRole('button', { name: 'Try again' })).toBeTruthy();
    // No dialog was opened for this kind, so the message cannot have come from `NewCardForm`.
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('shows a failed create inside the dialog exactly once for a kind with fields', async () => {
    setup({ createFails: true });
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(document.querySelectorAll('[data-nc-new-card-error]')).toHaveLength(1); });
    expect(document.querySelectorAll('[data-nc-operation-feedback]')).toHaveLength(0);
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

/* #2131 S2: one add-card intent is one `Idempotency-Key`. The fake server joins a retry under a stored key to its card,
 * so "one card" below is the kernel's answer to two attempts under one key, not a count of POSTs. */
describe('a keyed card create', () => {
  const keyOf = (request: ApiRequest | undefined) => request?.headers?.['Idempotency-Key'];
  const reused: ApiTransportResponse = { status: 409, statusText: 'Conflict', body: {
    error: 'This card request was already used for a different card.', code: 'idempotency_key_reused',
  } };
  const invalid: ApiTransportResponse = { status: 400, statusText: 'Bad Request', body: {
    error: 'The card request key is not valid.', code: 'idempotency_key_invalid',
  } };
  /* Only a create the kernel refused before committing anything answers `conflict` on these routes. */
  const conflict: ApiTransportResponse = { status: 409, statusText: 'Conflict', body: {
    error: 'The track is closed.', code: 'conflict',
  } };
  const unprocessable: ApiTransportResponse = { status: 422, statusText: 'Unprocessable Entity', body: {
    error: 'The theme is missing.', code: 'unprocessable',
  } };
  /* A create that failed for good after its commit; a retry under its key is answered the same. */
  const failed: ApiTransportResponse = { status: 500, statusText: 'Internal Server Error', body: {
    error: 'the daemon did not start.', code: 'operation_failed',
  } };

  async function fillTitle(title: string) {
    const field = await screen.findByRole('textbox', { name: 'Title' });
    await userEvent.clear(field);
    if (title !== '') await userEvent.type(field, title);
  }

  it('resends a terminal create whose answer was lost under the same key and body, and makes one card', async () => {
    const { posts, cards } = setup({ answers: ['lost'] });
    await pickKind('terminal');
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('Creating the terminal card is unconfirmed.');
    await userEvent.click(within(alert).getByRole('button', { name: 'Try again' }));
    await waitFor(() => { expect(screen.queryByRole('alert')).toBeNull(); });
    const [first, second] = posts();
    expect(posts()).toHaveLength(2);
    expect(first?.path).toBe('/api/tracks/w1/terminal-cards');
    expect(keyOf(first)).toMatch(/^[0-9a-f-]{36}$/);
    expect(keyOf(second)).toBe(keyOf(first));
    expect(second?.body).toEqual(first?.body);
    expect(cards).toHaveLength(1);
  });

  it('resends the first attempt’s body on Try again, even after the theme changed in between', async () => {
    const { posts, cards } = setup({ answers: ['lost'] });
    await pickKind('codex');
    await fillTitle('Build');
    await userEvent.click(screen.getByRole('button', { name: 'Create codex' }));
    const banner = await waitFor(() => {
      const shown = document.querySelector<HTMLElement>('[data-nc-new-card-error]');
      expect(shown?.textContent).toContain('Creating the codex card is unconfirmed.');
      return shown!;
    });
    const root = document.documentElement;
    root.dataset.theme = root.dataset.theme === 'light' ? 'dark' : 'light';
    await userEvent.click(within(banner).getByRole('button', { name: 'Try again' }));
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    const [first, second] = posts();
    expect(first?.path).toBe('/api/tracks/w1/codex-cards');
    expect(keyOf(second)).toBe(keyOf(first));
    expect(second?.body).toEqual(first?.body);
    expect(cards).toHaveLength(1);
  });

  it('keeps the key when Create is pressed again on the same draft, and mints a new one for a changed draft', async () => {
    const { posts } = setup({ answers: ['lost', 'lost'] });
    await pickKind('codex');
    await fillTitle('Build');
    await userEvent.click(screen.getByRole('button', { name: 'Create codex' }));
    await screen.findByRole('button', { name: 'Try again' });
    await userEvent.click(screen.getByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    await screen.findByRole('button', { name: 'Try again' });
    await fillTitle('Deploy');
    await userEvent.click(screen.getByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(posts()).toHaveLength(3); });
    const [first, second, third] = posts();
    expect(keyOf(second)).toBe(keyOf(first));
    expect(keyOf(third)).not.toBe(keyOf(first));
    expect(third?.body).toMatchObject({ title: 'Deploy' });
  });

  it('keeps the unknown create, and its Try again, after the dialog closes', async () => {
    const { posts, cards } = setup({ answers: ['lost'] });
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await screen.findByRole('button', { name: 'Try again' });
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('Creating the codex card is unconfirmed.');
    await userEvent.click(within(alert).getByRole('button', { name: 'Try again' }));
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    expect(keyOf(posts()[1])).toBe(keyOf(posts()[0]));
    expect(cards).toHaveLength(1);
  });

  it.each([
    ['a reused key', reused, 'This card request was already used for a different card.'],
    ['an invalid key', invalid, 'The card request key is not valid.'],
    ['a refused body', unprocessable, 'The theme is missing.'],
    ['a create refused before it committed', conflict, 'The track is closed.'],
    ['a create that failed for good under its key', failed, 'the daemon did not start.'],
  ])('reads %s as a final refusal with the server’s reason: no Try again, and the next press mints a new key', async (_name, answer, reason) => {
    const { posts } = setup({ answers: [answer] });
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await waitFor(() => {
      expect(document.querySelector('[data-nc-new-card-error]')?.textContent).toBe(reason);
    });
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    expect(keyOf(posts()[1])).not.toBe(keyOf(posts()[0]));
  });

  /* #2175: a create the kernel stopped part way is never driven again, and a retry under its key only replays this. */
  it('reads a create that stopped part way as final: the card may exist, no Try again, and the next press mints a new key', async () => {
    const stuck: ApiTransportResponse = { status: 500, statusText: 'Internal Server Error', body: {
      error: 'operation drive failed: the daemon stopped answering', code: 'operation_stuck',
    } };
    const { posts } = setup({ answers: [stuck] });
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await waitFor(() => {
      expect(document.querySelector('[data-nc-new-card-error]')?.textContent)
        .toBe('Creating the codex card stopped part way, so the card may exist. Check the track before creating another.');
    });
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    expect(keyOf(posts()[1])).not.toBe(keyOf(posts()[0]));
  });

  /* Picking the same fieldless kind again is the natural retry: it continues the held intent, as Try again does. */
  it('resends the held key and body when the same fieldless kind is picked again', async () => {
    const { posts, cards } = setup({ answers: ['lost'] });
    await pickKind('terminal');
    await screen.findByRole('button', { name: 'Try again' });
    await pickKind('terminal');
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    expect(keyOf(posts()[1])).toBe(keyOf(posts()[0]));
    expect(posts()[1]?.body).toEqual(posts()[0]?.body);
    expect(cards).toHaveLength(1);
  });

  it('starts a new intent, under a new key, when a different kind is picked', async () => {
    const { posts } = setup({ answers: ['lost'] });
    await pickKind('terminal');
    await screen.findByRole('button', { name: 'Try again' });
    await pickKind('codex');
    await userEvent.click(await screen.findByRole('button', { name: 'Create codex' }));
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    expect(posts()[1]?.path).toBe('/api/tracks/w1/codex-cards');
    expect(keyOf(posts()[1])).not.toBe(keyOf(posts()[0]));
  });

  /* Offline, a press is refused before anything is sent (`NotSentError`). */
  it('drops a fresh intent that could not be sent: a refusal with no Try again', async () => {
    const { posts } = setup();
    await screen.findByRole('button', { name: 'Add card' });
    onlineManager.setOnline(false);
    await pickKind('terminal');
    expect(within(await screen.findByRole('alert')).getByText('The terminal card was not created.')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(posts()).toHaveLength(0);
  });

  it('keeps an unknown intent whose Try again could not be sent, and resends its key once back online', async () => {
    const { posts, cards } = setup({ answers: ['lost'] });
    await pickKind('terminal');
    await screen.findByRole('button', { name: 'Try again' });
    onlineManager.setOnline(false);
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    const alert = await screen.findByRole('alert');
    await waitFor(() => { expect(screen.queryByRole('button', { name: 'Try again' })).not.toBeNull(); });
    expect(alert.textContent).toContain('Creating the terminal card is unconfirmed.');
    expect(posts()).toHaveLength(1);
    onlineManager.setOnline(true);
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    expect(keyOf(posts()[1])).toBe(keyOf(posts()[0]));
    expect(cards).toHaveLength(1);
  });

  /* #2131 S4: the generic create takes a key too, so a plugin card's Try again joins the card its first attempt made. */
  it('resends a plugin card create whose answer was lost under the same key and body, and makes one card', async () => {
    const { posts, cards } = setup({ answers: ['lost'] });
    await pickKind('file');
    await userEvent.click(await screen.findByRole('button', { name: 'File or folder' }));
    await userEvent.click(await screen.findByRole('option', { name: 'notes.md' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Create file' }));
    const banner = await waitFor(() => {
      const shown = document.querySelector<HTMLElement>('[data-nc-new-card-error]');
      expect(shown?.textContent).toContain('Creating the file card is unconfirmed.');
      return shown!;
    });
    await userEvent.click(within(banner).getByRole('button', { name: 'Try again' }));
    await waitFor(() => { expect(posts()).toHaveLength(2); });
    const [first, second] = posts();
    expect(first?.path).toBe('/api/tracks/w1/cards');
    expect(keyOf(first)).toMatch(/^[0-9a-f-]{36}$/);
    expect(keyOf(second)).toBe(keyOf(first));
    expect(second?.body).toEqual(first?.body);
    expect(cards).toHaveLength(1);
  });
});
