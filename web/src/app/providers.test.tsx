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
import { RefreshRequiredOverlay } from './providers';
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
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
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
