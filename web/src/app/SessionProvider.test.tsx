// Tests for the issue #189 SessionProvider gate.
//
// What we're proving:
//   1. First mount runs whoami; while in flight nothing is rendered.
//   2. whoami returning the owner payload mounts `children`.
//   3. whoami returning 401 mounts `<LoginPage />` instead — children
//      must NOT render (the load-bearing invariant for route loaders).
//   4. A `fireUnauthorized()` after a successful mount wipes the React
//      Query cache, drops the WS cursor key, and bounces back to
//      `<LoginPage />` without ever mounting children again.
//
// We stub `fetch` end-to-end so the SessionProvider's `whoami()` call
// hits a mock; no real network. The QueryClientProvider is needed because
// SessionProvider's `onUnauthorized` listener pulls `useQueryClient()` —
// we instantiate a fresh one per test.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor, act } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { SessionProvider } from './SessionProvider';
import {
  fireUnauthorized,
  _resetUnauthorizedListenersForTest,
} from '../api/onUnauthorized';

function mockWhoamiResponse(status: number, body?: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: status >= 200 && status < 300,
      status,
      statusText: status === 401 ? 'Unauthorized' : 'OK',
      json: async () => body ?? {},
    } as unknown as Response),
  );
}

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
}

beforeEach(() => {
  _resetUnauthorizedListenersForTest();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('SessionProvider', () => {
  it('renders nothing while whoami is in flight', () => {
    // Stub fetch with a never-resolving promise so we can observe the
    // pre-resolution mount tick.
    vi.stubGlobal(
      'fetch',
      vi.fn(() => new Promise(() => {})),
    );
    const qc = makeClient();
    const { container } = render(
      <QueryClientProvider client={qc}>
        <SessionProvider>
          <div data-testid="children">app</div>
        </SessionProvider>
      </QueryClientProvider>,
    );
    expect(container.textContent ?? '').toBe('');
  });

  it('renders children after a successful whoami', async () => {
    mockWhoamiResponse(200, {
      userId: 'local-owner',
      displayName: 'Owner',
      role: 'owner',
      sessionId: 'abc',
    });
    const qc = makeClient();
    render(
      <QueryClientProvider client={qc}>
        <SessionProvider>
          <div data-testid="children">app</div>
        </SessionProvider>
      </QueryClientProvider>,
    );
    await waitFor(() => {
      expect(screen.getByTestId('children')).toBeInTheDocument();
    });
  });

  it('renders LoginPage on 401 and does NOT mount children', async () => {
    mockWhoamiResponse(401);
    const qc = makeClient();
    render(
      <QueryClientProvider client={qc}>
        <SessionProvider>
          <div data-testid="children">app</div>
        </SessionProvider>
      </QueryClientProvider>,
    );
    // The LoginPage's "Sign in." heading must appear; the wrapped
    // children must not.
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: /sign in/i })).toBeInTheDocument();
    });
    expect(screen.queryByTestId('children')).toBeNull();
  });

  it('fireUnauthorized clears cache, cursor, and flips to LoginPage', async () => {
    mockWhoamiResponse(200, {
      userId: 'local-owner',
      displayName: 'Owner',
      role: 'owner',
      sessionId: 'abc',
    });
    const qc = makeClient();
    // Seed a value the listener should wipe.
    qc.setQueryData(['some-key'], { value: 1 });
    // Seed the WS cursor key the listener should drop.
    localStorage.setItem('calm:sync:cursor', '42');

    render(
      <QueryClientProvider client={qc}>
        <SessionProvider>
          <div data-testid="children">app</div>
        </SessionProvider>
      </QueryClientProvider>,
    );
    await waitFor(() => {
      expect(screen.getByTestId('children')).toBeInTheDocument();
    });

    // Fire the global 401 channel — SessionProvider's listener should
    // wipe the cache + cursor + flip back to LoginPage. The listener is
    // dispatched via `queueMicrotask` so we wrap in `act` and await a
    // microtask flush before assertions.
    await act(async () => {
      fireUnauthorized();
      // Yield the microtask queue.
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: /sign in/i })).toBeInTheDocument();
    });
    expect(screen.queryByTestId('children')).toBeNull();
    // Cache was cleared — `getQueryData` returns undefined.
    expect(qc.getQueryData(['some-key'])).toBeUndefined();
    // WS cursor key was dropped.
    expect(localStorage.getItem('calm:sync:cursor')).toBeNull();
  });

  it('renders error state when whoami throws (non-401)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: false,
        status: 500,
        statusText: 'Server Error',
        json: async () => ({}),
      } as unknown as Response),
    );
    const qc = makeClient();
    render(
      <QueryClientProvider client={qc}>
        <SessionProvider>
          <div data-testid="children">app</div>
        </SessionProvider>
      </QueryClientProvider>,
    );
    await waitFor(() => {
      expect(screen.getByText(/Cannot reach server/i)).toBeInTheDocument();
    });
    expect(screen.queryByTestId('children')).toBeNull();
  });
});
