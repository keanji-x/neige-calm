// Tests for the frontend ↔ backend skew enforcement wired through
// `AppProviders` (see issue #45 / `docs/upgrade-stability.md`).
//
// What we're proving:
//   1. `RefreshRequiredOverlay` renders the both-versions message and a
//      refresh button. The button is wired to `window.location.reload()`.
//   2. When `/api/version` returns a `minWebCompatVersion` ahead of this
//      bundle's `WEB_COMPAT_VERSION`, the overlay paints over the app
//      tree and the inner children are NOT rendered (hard block).
//   3. When the server is compatible, children render normally.
//
// We mock the global `fetch` instead of pulling in `msw` — the surface
// is one endpoint and the providers contract is best validated against
// the real `fetchServerVersion` flow.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';

// Issue #198 concern 1: mock EventBridge BEFORE importing providers so we
// can assert that ServerCompatGate only mounts the bridge once compat is
// confirmed. The bridge's real implementation opens a WebSocket on
// `useEffect`; a probe component lets us verify mount/unmount cleanly.
const eventBridgeMountSpy = vi.fn();
vi.mock('./eventBridge', () => ({
  EventBridge: (props: { syncEventVersion: number }) => {
    eventBridgeMountSpy(props);
    return <div data-testid="event-bridge-mock" data-version={props.syncEventVersion} />;
  },
}));

import {
  DB_INSTANCE_ID_STORAGE_KEY,
  RefreshRequiredOverlay,
  ServerCompatGate,
  retryUnless401,
} from './providers';
import { IDB_DB_NAME } from '../api/persistConfig';
import { CalmApiError } from '../api/calm';
import {
  WEB_COMPAT_VERSION,
  isCompatible,
  type ServerVersionInfo,
} from '../api/version';

// -- helpers ------------------------------------------------------------

function makeServerInfo(over: Partial<ServerVersionInfo> = {}): ServerVersionInfo {
  return {
    kernelVersion: '0.1.0',
    apiVersion: '1',
    syncEventVersion: 1,
    mcpProtocolVersion: '2025-11-25',
    minWebCompatVersion: WEB_COMPAT_VERSION,
    buildSha: null,
    dbInstanceId: '00000000-0000-4000-8000-000000000000',
    ...over,
  };
}

function mockFetchVersion(body: ServerVersionInfo): void {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      statusText: 'OK',
      json: async () => body,
    } as unknown as Response),
  );
}

/** Minimal stand-in for `ServerCompatGate` that we can render under a
 *  QueryClient we control. The real gate lives in `providers.tsx`; to
 *  avoid pulling the whole `PersistQueryClientProvider` + IndexedDB
 *  stack into this test (already covered in `persistConfig.test.tsx`),
 *  we exercise the same logic against a bare QueryClient. The component
 *  itself is the contract being tested. */
function TestCompatGate({ children }: { children: React.ReactNode }) {
  const q = useQuery<ServerVersionInfo>({
    queryKey: ['server-version'],
    queryFn: async () => {
      const res = await fetch('/api/version');
      return (await res.json()) as ServerVersionInfo;
    },
    staleTime: 0,
    retry: false,
  });
  if (q.data && !isCompatible(q.data)) {
    return <RefreshRequiredOverlay server={q.data} />;
  }
  return <>{children}</>;
}

function renderWithClient(ui: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

// -- tests --------------------------------------------------------------

beforeEach(() => {
  cleanup();
  eventBridgeMountSpy.mockClear();
  // Ensure cross-test isolation for the DB-instance check.
  try {
    localStorage.removeItem(DB_INSTANCE_ID_STORAGE_KEY);
    localStorage.removeItem('calm:sync:cursor');
  } catch {
    /* jsdom env always has localStorage; guard anyway */
  }
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  try {
    localStorage.removeItem(DB_INSTANCE_ID_STORAGE_KEY);
    localStorage.removeItem('calm:sync:cursor');
  } catch {
    /* */
  }
});

describe('RefreshRequiredOverlay', () => {
  it('renders both versions and a refresh button', () => {
    const server = makeServerInfo({ minWebCompatVersion: WEB_COMPAT_VERSION + 5 });
    render(<RefreshRequiredOverlay server={server} />);

    // Accessible name comes from the shared `<Dialog>` primitive's title
    // (Slice 1 of #60: the overlay no longer hand-rolls its own role+label).
    expect(screen.getByRole('dialog', { name: 'Please refresh' })).toBeInTheDocument();
    expect(screen.getByText('Please refresh')).toBeInTheDocument();
    // The user needs to see both numbers so an operator-style log of
    // "compat v3 vs v1" is obvious.
    expect(
      screen.getByText(new RegExp(`compat v${server.minWebCompatVersion}`)),
    ).toBeInTheDocument();
    expect(
      screen.getByText(new RegExp(`compat v${WEB_COMPAT_VERSION}`)),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /refresh now/i })).toBeInTheDocument();
  });

  it('refresh button calls window.location.reload()', async () => {
    // `window.location` is non-writable in jsdom; use defineProperty to
    // swap in a spy without tripping the standard mock-strictness rule.
    const reload = vi.fn();
    Object.defineProperty(window, 'location', {
      configurable: true,
      value: { ...window.location, reload },
    });

    const server = makeServerInfo({ minWebCompatVersion: WEB_COMPAT_VERSION + 1 });
    render(<RefreshRequiredOverlay server={server} />);

    await userEvent.click(screen.getByRole('button', { name: /refresh now/i }));
    expect(reload).toHaveBeenCalledTimes(1);
  });
});

describe('ServerCompatGate (via TestCompatGate)', () => {
  it('renders children when server is compatible', async () => {
    mockFetchVersion(makeServerInfo({ minWebCompatVersion: WEB_COMPAT_VERSION }));

    renderWithClient(
      <TestCompatGate>
        <div data-testid="app">app body</div>
      </TestCompatGate>,
    );

    await waitFor(() => {
      expect(screen.getByTestId('app')).toBeInTheDocument();
    });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('renders the refresh modal and hides children when frontend is below the server minimum', async () => {
    mockFetchVersion(
      makeServerInfo({ minWebCompatVersion: WEB_COMPAT_VERSION + 1 }),
    );

    renderWithClient(
      <TestCompatGate>
        <div data-testid="app">app body</div>
      </TestCompatGate>,
    );

    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });
    expect(screen.getByText('Please refresh')).toBeInTheDocument();
    expect(screen.queryByTestId('app')).not.toBeInTheDocument();
  });
});

// --- DB instance id (cache buster) -------------------------------------
//
// These tests exercise the real `ServerCompatGate` (not the
// `TestCompatGate` mirror) under a bare QueryClient, because the
// db-instance-id branch is the contract under test and we want the
// production code path to be the thing that runs. We mock
// `window.location.reload` (jsdom makes `location` non-writable so we
// `defineProperty` instead of a plain assignment) and `indexedDB.
// deleteDatabase` so the test stays hermetic.

const ID_A = '11111111-1111-4111-8111-111111111111';
const ID_B = '22222222-2222-4222-8222-222222222222';

function installLocationReloadSpy() {
  const reload = vi.fn();
  Object.defineProperty(window, 'location', {
    configurable: true,
    value: { ...window.location, reload },
  });
  return reload;
}

function installIndexedDBSpy() {
  const deleteDatabase = vi.fn().mockReturnValue({} as IDBOpenDBRequest);
  Object.defineProperty(window, 'indexedDB', {
    configurable: true,
    value: { deleteDatabase },
  });
  return deleteDatabase;
}

// --- retryUnless401 -----------------------------------------------------
//
// 401 means the session cookie is gone; `request()` in `api/calm.ts` has
// already fired `onUnauthorized`, the SessionProvider is about to bounce
// us back to LoginPage, and a retry just stacks up a doomed second call.
// Every non-auth failure still gets the standard "one retry" budget.

describe('retryUnless401', () => {
  it('does not retry on CalmApiError with status 401', () => {
    const err = new CalmApiError(401, 'unauthorized', 'no session');
    expect(retryUnless401(0, err)).toBe(false);
    // Even later in the retry budget (defensive — failureCount should
    // never get above 0 for a 401, but the policy must hold either way).
    expect(retryUnless401(5, err)).toBe(false);
  });

  it('retries once on a 500 CalmApiError', () => {
    const err = new CalmApiError(500, 'internal', 'boom');
    expect(retryUnless401(0, err)).toBe(true);
    expect(retryUnless401(1, err)).toBe(false);
  });

  it('retries once on a non-CalmApiError (network failure)', () => {
    const err = new Error('Failed to fetch');
    expect(retryUnless401(0, err)).toBe(true);
    expect(retryUnless401(1, err)).toBe(false);
  });
});

describe('ServerCompatGate — dbInstanceId cache bust', () => {
  it('stores the id on first boot and renders children (no clear, no reload)', async () => {
    expect(localStorage.getItem(DB_INSTANCE_ID_STORAGE_KEY)).toBeNull();

    mockFetchVersion(makeServerInfo({ dbInstanceId: ID_A }));
    const reload = installLocationReloadSpy();
    const deleteIDB = installIndexedDBSpy();

    renderWithClient(
      <ServerCompatGate>
        <div data-testid="app">app body</div>
      </ServerCompatGate>,
    );

    // First the children paint (loading state still renders them, since
    // we only block on `isCompatible`). Then the useEffect runs once
    // the version query resolves and stores the id.
    await waitFor(() => {
      expect(localStorage.getItem(DB_INSTANCE_ID_STORAGE_KEY)).toBe(ID_A);
    });
    expect(screen.getByTestId('app')).toBeInTheDocument();
    expect(reload).not.toHaveBeenCalled();
    expect(deleteIDB).not.toHaveBeenCalled();
  });

  it('renders children without reloading when the stored id matches', async () => {
    localStorage.setItem(DB_INSTANCE_ID_STORAGE_KEY, ID_A);
    // Pre-existing WS cursor must NOT be wiped on the matching path —
    // we'd lose every event since boot otherwise.
    localStorage.setItem('calm:sync:cursor', '42');

    mockFetchVersion(makeServerInfo({ dbInstanceId: ID_A }));
    const reload = installLocationReloadSpy();
    const deleteIDB = installIndexedDBSpy();

    renderWithClient(
      <ServerCompatGate>
        <div data-testid="app">app body</div>
      </ServerCompatGate>,
    );

    await waitFor(() => {
      expect(screen.getByTestId('app')).toBeInTheDocument();
    });
    // Even after the query has resolved + the effect has had a chance
    // to run, nothing about persistent state should change on the
    // matching path.
    expect(localStorage.getItem(DB_INSTANCE_ID_STORAGE_KEY)).toBe(ID_A);
    expect(localStorage.getItem('calm:sync:cursor')).toBe('42');
    expect(reload).not.toHaveBeenCalled();
    expect(deleteIDB).not.toHaveBeenCalled();
  });

  it('clears qc / WS cursor / IDB and reloads when the id has changed', async () => {
    // Simulate a previous server boot that minted ID_A.
    localStorage.setItem(DB_INSTANCE_ID_STORAGE_KEY, ID_A);
    localStorage.setItem('calm:sync:cursor', '999');

    // Now the server reports ID_B — the DB was reset under us.
    mockFetchVersion(makeServerInfo({ dbInstanceId: ID_B }));
    const reload = installLocationReloadSpy();
    const deleteIDB = installIndexedDBSpy();

    // Render with a client that has a known query in cache, so we can
    // verify `qc.clear()` actually fires.
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: 0 } },
    });
    client.setQueryData(['coves'], [{ id: 'stale-cove-from-previous-db' }]);
    expect(client.getQueryData(['coves'])).toBeDefined();

    render(
      <QueryClientProvider client={client}>
        <ServerCompatGate>
          <div data-testid="app">app body</div>
        </ServerCompatGate>
      </QueryClientProvider>,
    );

    // Wait for the bust path to run.
    await waitFor(() => {
      expect(reload).toHaveBeenCalledTimes(1);
    });

    // All three persisted artifacts were cleared / rewritten.
    expect(client.getQueryData(['coves'])).toBeUndefined();
    expect(localStorage.getItem('calm:sync:cursor')).toBeNull();
    expect(deleteIDB).toHaveBeenCalledWith(IDB_DB_NAME);
    expect(localStorage.getItem(DB_INSTANCE_ID_STORAGE_KEY)).toBe(ID_B);

    // Children are NOT rendered during the in-flight reload — we paint
    // null so the user doesn't see an empty-cache flash.
    expect(screen.queryByTestId('app')).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Issue #198, concern 1: EventBridge gating.
//
// The bridge opens `/api/events` on mount, so an incompatible-frontend
// scenario MUST NOT mount it — otherwise an old bundle would still talk
// to a kernel it can't parse. We assert:
//   1. Incompatible server (minWebCompatVersion > WEB_COMPAT_VERSION):
//      modal renders, bridge does NOT mount.
//   2. Compatible server: bridge mounts with the server-declared
//      `syncEventVersion`.
//   3. Version query in flight (`q.data` is undefined): bridge stays
//      unmounted so no WS attempt happens before the gate verdict.
// ---------------------------------------------------------------------------

describe('ServerCompatGate — EventBridge gating (issue #198 concern 1)', () => {
  it('does NOT mount EventBridge when the frontend is below the server minimum', async () => {
    mockFetchVersion(
      makeServerInfo({ minWebCompatVersion: WEB_COMPAT_VERSION + 1 }),
    );

    renderWithClient(
      <ServerCompatGate>
        <div data-testid="app">app body</div>
      </ServerCompatGate>,
    );

    // Wait for the version query to resolve and the modal to paint.
    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });
    // Bridge must NOT mount — that's the load-bearing assertion.
    expect(eventBridgeMountSpy).not.toHaveBeenCalled();
    expect(screen.queryByTestId('event-bridge-mock')).not.toBeInTheDocument();
    // App body is hidden behind the modal.
    expect(screen.queryByTestId('app')).not.toBeInTheDocument();
  });

  it('mounts EventBridge with syncEventVersion when the server is compatible', async () => {
    mockFetchVersion(
      makeServerInfo({
        minWebCompatVersion: WEB_COMPAT_VERSION,
        syncEventVersion: 3,
      }),
    );

    renderWithClient(
      <ServerCompatGate>
        <div data-testid="app">app body</div>
      </ServerCompatGate>,
    );

    await waitFor(() => {
      expect(screen.getByTestId('event-bridge-mock')).toBeInTheDocument();
    });
    // The bridge received the server-declared eventVersion ceiling.
    expect(screen.getByTestId('event-bridge-mock')).toHaveAttribute(
      'data-version',
      '3',
    );
    expect(eventBridgeMountSpy).toHaveBeenCalledWith({ syncEventVersion: 3 });
    // And the app body renders alongside it.
    expect(screen.getByTestId('app')).toBeInTheDocument();
  });

  it('keeps EventBridge unmounted while the version query is still in flight', () => {
    // `fetch` never resolves in this test — simulates the cold-start window
    // between mount and first /api/version response. The bridge must NOT
    // mount in this state, even though the gate optimistically renders
    // children so cached routes can paint from the persisted cache.
    vi.stubGlobal(
      'fetch',
      vi.fn(() => new Promise(() => {})),
    );

    renderWithClient(
      <ServerCompatGate>
        <div data-testid="app">app body</div>
      </ServerCompatGate>,
    );

    // Synchronously: children render (so persisted-cache paint isn't
    // blocked on the version query), but the bridge stays unmounted.
    expect(screen.getByTestId('app')).toBeInTheDocument();
    expect(screen.queryByTestId('event-bridge-mock')).not.toBeInTheDocument();
    expect(eventBridgeMountSpy).not.toHaveBeenCalled();
  });
});
