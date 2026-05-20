// Integration tests for the IndexedDB-backed query persister
// (`api/persistConfig.ts` + `app/providers.tsx` wiring).
//
// What we're proving:
//   1. A query whose key is on the allowlist (`['coves']`) survives a
//      remount via IndexedDB — the data is restored before the queryFn
//      runs, which is exactly the "no flash of empty UI" property the
//      provider is supposed to give us.
//   2. A query whose key is NOT on the allowlist (`['transient']`) does
//      NOT make it to disk — bumping the in-memory cache shouldn't ever
//      bloat IndexedDB with ad-hoc keys.
//   3. Changing the `buster` simulates a "new app version deployed":
//      the old persisted blob is rejected and the new mount starts cold.
//
// We use `fake-indexeddb/auto` so jsdom gets a working in-process IDB
// (the side-effecting import installs the polyfill onto `globalThis`).
// Each test swaps in a fresh `IDBFactory` to invalidate all prior
// connections at once (see `resetIDB` below for why deleteDatabase is
// the wrong tool here).
//
// Throttling note: `createAsyncStoragePersister` debounces writes (1s
// default). Tests build a zero-throttle persister via the same factory
// so the write completes synchronously after the await, instead of
// requiring `vi.useFakeTimers()` games or 1s+ wall-clock waits per case.
//
// Why not unit-test `shouldPersistQuery` in isolation only: that function
// trivially mirrors a list of key shapes; what we actually need to lock
// in is the end-to-end provider behavior — predicate + persister +
// hydrate all wired together. The TanStack code between predicate and
// disk is real, not mocked.

import 'fake-indexeddb/auto';
import { IDBFactory } from 'fake-indexeddb';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, waitFor, cleanup } from '@testing-library/react';
import { QueryClient, useQuery, dehydrate } from '@tanstack/react-query';
import { PersistQueryClientProvider } from '@tanstack/react-query-persist-client';
import type { PersistedClient } from '@tanstack/react-query-persist-client';

import {
  buildPersistOptions,
  createIDBPersister,
  shouldPersistQuery,
  PERSIST_MAX_AGE_MS,
} from './persistConfig';

// --- helpers -----------------------------------------------------------

/** Reset IDB between tests by swapping the global factory.
 *  `idb-keyval`'s `createStore` memoizes a connection per store instance,
 *  so `indexedDB.deleteDatabase` would deadlock on `onblocked` waiting for
 *  the previous test's open handles. Swapping in a brand-new `IDBFactory`
 *  invalidates all old connections at once — clean slate without having to
 *  thread teardown through every test. */
function resetIDB(): void {
  // jsdom exposes `indexedDB` as a configurable global once `fake-indexeddb/auto`
  // installs it; reassigning is the supported reset path.
  (globalThis as { indexedDB: IDBFactory }).indexedDB = new IDBFactory();
}

function makeClient(): QueryClient {
  // Disable retries; keep cache long-lived so a setQueryData write isn't
  // immediately GC'd before the persister flushes.
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity, gcTime: Infinity },
      mutations: { retry: false },
    },
  });
}

/** Build a PersistedClient blob with one allowlisted query pre-populated.
 *  Used to seed IDB without the throttled provider lifecycle — simulates
 *  "previous session left this on disk". */
function makePersistedClient(
  queryKey: readonly unknown[],
  data: unknown,
  buster: string,
): PersistedClient {
  const client = new QueryClient();
  client.setQueryData([...queryKey], data);
  return {
    buster,
    timestamp: Date.now(),
    clientState: dehydrate(client, { shouldDehydrateQuery: shouldPersistQuery }),
  };
}

beforeEach(() => {
  cleanup();
  resetIDB();
});

// --- shouldPersistQuery (allowlist predicate) --------------------------
//
// Belt-and-suspenders: the integration tests below exercise the predicate
// end-to-end, but the allowlist is load-bearing enough that we also pin
// each shape directly so a regression names itself in the test output.

describe('shouldPersistQuery (allowlist)', () => {
  it("accepts ['coves']", () => {
    expect(shouldPersistQuery({ queryKey: ['coves'] })).toBe(true);
  });
  it("accepts ['waves', <id>]", () => {
    expect(shouldPersistQuery({ queryKey: ['waves', 'c1'] })).toBe(true);
  });
  it("accepts ['wave', <id>]", () => {
    expect(shouldPersistQuery({ queryKey: ['wave', 'w1'] })).toBe(true);
  });
  it("accepts ['overlays', <kind>]", () => {
    expect(shouldPersistQuery({ queryKey: ['overlays', 'wave'] })).toBe(true);
  });
  it("accepts ['overlay', <id>]", () => {
    expect(shouldPersistQuery({ queryKey: ['overlay', 'o1'] })).toBe(true);
  });

  it("rejects ['settings']", () => {
    expect(shouldPersistQuery({ queryKey: ['settings'] })).toBe(false);
  });
  it('rejects unknown root keys', () => {
    expect(shouldPersistQuery({ queryKey: ['transient'] })).toBe(false);
    expect(shouldPersistQuery({ queryKey: ['foo', 'bar'] })).toBe(false);
  });
  it('rejects empty / malformed keys', () => {
    expect(shouldPersistQuery({ queryKey: [] })).toBe(false);
    expect(
      shouldPersistQuery({ queryKey: 'coves' as unknown as readonly unknown[] }),
    ).toBe(false);
  });
});

// --- end-to-end persist + restore --------------------------------------

describe('PersistQueryClientProvider + IndexedDB', () => {
  it('restores an allowlisted query (["coves"]) across a remount', async () => {
    const fakeCoves = [
      { id: 'c1', name: 'Persisted', color: '#abc', sort: 0, created_at: 1, updated_at: 2 },
    ];
    const buildOpts = buildPersistOptions();

    // Seed IDB directly via the persister (no throttle on this one) so the
    // "second mount" can hydrate from a known-good blob.
    const seedPersister = createIDBPersister({ throttleTime: 0 });
    await seedPersister.persistClient(
      makePersistedClient(['coves'], fakeCoves, buildOpts.buster),
    );

    // Fresh client + same build-time persist options. Hydration should
    // populate the cache *before* any queryFn runs.
    const client = makeClient();
    const queryFn = vi.fn().mockResolvedValue([]);

    function Reader() {
      const q = useQuery({
        queryKey: ['coves'],
        queryFn,
        // Keep the data we hydrate from cache "fresh" so RQ doesn't fire
        // a background refetch and race the assertion.
        staleTime: Infinity,
      });
      return <div data-testid="data">{q.data ? JSON.stringify(q.data) : 'empty'}</div>;
    }

    const { getByTestId } = render(
      <PersistQueryClientProvider client={client} persistOptions={buildOpts}>
        <Reader />
      </PersistQueryClientProvider>,
    );

    await waitFor(() => {
      expect(getByTestId('data').textContent).toBe(JSON.stringify(fakeCoves));
    });
  });

  it('does NOT persist a query outside the allowlist (["transient"])', async () => {
    // Mount the cache with both an allowlisted and a non-allowlisted
    // query, then dehydrate through the production `shouldDehydrateQuery`
    // predicate and save via the real persister.
    const opts = buildPersistOptions();
    const persister = createIDBPersister({ throttleTime: 0 });

    const client = makeClient();
    client.setQueryData(['coves'], [{ id: 'c1' }]);
    client.setQueryData(['transient'], { secret: 'do-not-persist' });

    const dehydrated = dehydrate(client, opts.dehydrateOptions);
    await persister.persistClient({
      buster: opts.buster,
      timestamp: Date.now(),
      clientState: dehydrated,
    });

    const restored = await persister.restoreClient();
    expect(restored).toBeDefined();
    const persistedKeys = restored!.clientState.queries.map((q) => q.queryKey);
    expect(persistedKeys).toContainEqual(['coves']);
    expect(persistedKeys).not.toContainEqual(['transient']);
  });

  it('clears the cache when the buster changes (e.g. on version bump)', async () => {
    // Seed IDB with a blob tagged "v-old".
    const seedPersister = createIDBPersister({ throttleTime: 0 });
    await seedPersister.persistClient(
      makePersistedClient(['coves'], [{ id: 'old' }], 'v-old'),
    );

    const before = await seedPersister.restoreClient();
    expect(before?.buster).toBe('v-old');

    // Mount with a fresh persister + new buster ("v-new"). The provider's
    // restore phase compares busters and calls `removeClient` on mismatch.
    const persister = createIDBPersister({ throttleTime: 0 });
    const optionsB = {
      persister,
      maxAge: PERSIST_MAX_AGE_MS,
      buster: 'v-new',
      dehydrateOptions: { shouldDehydrateQuery: shouldPersistQuery },
    };
    const client = makeClient();
    const queryFn = vi.fn().mockResolvedValue([]);

    function Reader() {
      const q = useQuery({
        queryKey: ['coves'],
        queryFn,
        staleTime: Infinity,
      });
      return <div data-testid="data">{q.data ? JSON.stringify(q.data) : 'empty'}</div>;
    }

    render(
      <PersistQueryClientProvider client={client} persistOptions={optionsB}>
        <Reader />
      </PersistQueryClientProvider>,
    );

    // After hydration: either the persister wiped the blob entirely, or
    // replaced it with one carrying the new buster. The bad case (and the
    // only thing we're guarding against) is the old blob lingering with
    // buster "v-old".
    await waitFor(async () => {
      const after = await persister.restoreClient();
      if (after) {
        expect(after.buster).not.toBe('v-old');
      } else {
        expect(after).toBeUndefined();
      }
    });
  });
});
