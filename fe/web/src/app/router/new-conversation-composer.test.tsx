// @vitest-environment jsdom
// A conversation not created yet keeps its unsent words per Track, driven through the real router (#1923).
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { trackConversationCardId } from '../../../../core/domain/conversation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const track = (id: string, title: string, sort: number) => ({
  id, area_id: 'c1', title, sort, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2,
});
const TRACKS = [track('w1', 'Track one', 1), track('w2', 'Track two', 2)];

/** The whole router over a fake kernel whose create mints the row the key derives, and lists it afterwards. */
function renderApp() {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
  /* Reduced motion: a closed drawer unmounts at once instead of retracting with its last frame, which jsdom never finishes. */
  vi.stubGlobal('matchMedia', vi.fn((media: string) => ({
    matches: media === '(prefers-reduced-motion: reduce)', media, onchange: null,
    addEventListener: vi.fn(), removeEventListener: vi.fn(),
    addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
  })));
  const requests: ApiRequest[] = [];
  const minted = new Map<string, unknown[]>();
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      const conversations = /^\/api\/tracks\/(\w+)\/conversations$/.exec(request.path);
      if (conversations !== null) {
        const trackId = conversations[1];
        const rows = minted.get(trackId) ?? [];
        if (request.method !== 'POST') return Promise.resolve(ok(rows));
        const row = {
          id: trackConversationCardId(trackId, request.headers?.['Idempotency-Key'] ?? ''),
          trackId, title: null, kind: 'track-assistant', state: null, updatedAt: 99, lastTurnCompletedAt: null,
        };
        minted.set(trackId, [...rows, row]);
        return Promise.resolve({ status: 201, statusText: 'Created', body: row });
      }
      if (request.path === '/api/today/launchpad') {
        return Promise.resolve(ok({ track_id: 'lp', report_has_noninitial_content: false }));
      }
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok(TRACKS));
      const detail = TRACKS.find((candidate) => request.path === `/api/tracks/${candidate.id}`);
      if (detail !== undefined) {
        return Promise.resolve(ok({ track: detail, can_reopen: false, can_close: true, cards: [], overlays: [] }));
      }
      if (request.path === '/api/settings') return Promise.resolve(ok({}));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  render(
    <QueryClientProvider client={client}>
      <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return { requests, router };
}

async function openTrack(router: ReturnType<typeof renderApp>['router'], trackId: string) {
  await act(async () => { await router.navigate({ to: '/track/$trackId', params: { trackId } }); });
  await screen.findByText('No conversations yet.');
}

/** The `+`, which opens the "Untitled" drawer of a conversation not created yet. */
async function pressPlus() {
  fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
  await screen.findByRole('complementary', { name: 'Untitled' });
}

/** Close; the composer leaves the page with the drawer, so the next `+` mounts a fresh one. */
function closeDrawer() {
  fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  expect(document.querySelector('[data-nc-drawer]')).toBeNull();
}

const messageField = () => screen.getByRole('combobox', { name: 'Message' });

/* The composer is Astryx's contenteditable div: no value setter, so `change` throws. */
async function typeInto(text: string) {
  const field = messageField();
  field.textContent = text;
  const range = document.createRange();
  range.setStart(field.firstChild!, text.length);
  range.collapse(true);
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
  await act(async () => {
    fireEvent.input(field);
    await Promise.resolve();
  });
}

it('brings the words back when the drawer is closed and `+` is pressed again', async () => {
  const { requests } = renderApp();
  await screen.findByText('No conversations yet.');
  await pressPlus();
  await typeInto('Half a first message');
  closeDrawer();
  await pressPlus();
  await waitFor(() => expect(messageField().textContent).toBe('Half a first message'));
  expect(requests.filter((request) => request.method === 'POST')).toEqual([]);
});

it('keeps the words when the reader leaves the Track and comes back', async () => {
  const { router } = renderApp();
  await screen.findByText('No conversations yet.');
  await pressPlus();
  await typeInto('Written before leaving');
  await act(async () => { await router.navigate({ to: '/' }); });
  await waitFor(() => expect(screen.queryByRole('combobox', { name: 'Message' })).toBeNull());
  await openTrack(router, 'w1');
  await pressPlus();
  await waitFor(() => expect(messageField().textContent).toBe('Written before leaving'));
});

it('gives another Track its own empty field', async () => {
  const { router } = renderApp();
  await screen.findByText('No conversations yet.');
  await pressPlus();
  await typeInto('Only for track one');
  closeDrawer();
  await openTrack(router, 'w2');
  await pressPlus();
  expect(messageField().textContent).toBe('');
  await typeInto('Only for track two');
  closeDrawer();
  await openTrack(router, 'w1');
  await pressPlus();
  await waitFor(() => expect(messageField().textContent).toBe('Only for track one'));
});

it('leaves an empty field for the next `+` after the first message is sent', async () => {
  const { requests } = renderApp();
  await screen.findByText('No conversations yet.');
  await pressPlus();
  await typeInto('The first message');
  await act(async () => {
    fireEvent.keyDown(messageField(), { key: 'Enter' });
    await Promise.resolve();
  });
  await screen.findByRole('complementary', { name: 'Assistant' });
  expect(requests.filter((request) => request.method === 'POST').map((request) => request.body))
    .toEqual([{ text: 'The first message' }]);
  closeDrawer();
  await pressPlus();
  expect(messageField().textContent).toBe('');
});
