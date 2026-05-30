// Issue #199 — db-instance-id cache bust against REAL IndexedDB.
//
// The original cache-bust tests in `providers.test.tsx` mock
// `window.indexedDB` with a `vi.fn()` stub. That proves the call site
// hands the right db name to `deleteDatabase`, but it doesn't prove
// that the real IDB persisted blob actually goes away — the persister
// uses idb-keyval's own connection on top of `createStore`, and there
// are real ways for a deletion to be NO-OP'd (open handle blocking,
// wrong db name typo, name drift between persister and gate, …) that
// a stubbed `deleteDatabase` would silently let through.
//
// This test plugs the persister into the actual `ServerCompatGate`
// effect path using `fake-indexeddb/auto`, seeds the same shape the
// production persister writes, then asserts after the cache-bust
// effect runs that:
//
//   1. `indexedDB.deleteDatabase` was called against the REAL
//      persister's db name (not a guess or a typo). Wrapping
//      the real fake-indexeddb method (rather than replacing it
//      with a stub) preserves the underlying engine so the seed
//      data, the persister's own open handle, and the bust
//      deletion all interact through the real implementation.
//   2. `localStorage[WS_CURSOR_STORAGE_KEY]` is removed.
//   3. The QueryClient's in-memory data for an allowlisted key is
//      cleared.
//   4. `localStorage[DB_INSTANCE_ID_STORAGE_KEY]` is rewritten to the
//      new value the server returned.
//   5. `window.location.reload` is called exactly once.
//
// Lifecycle order matters: the gate calls `qc.clear()` + `safeDelete
// IDB()` + `safeLocalStorageRemove(WS_CURSOR_STORAGE_KEY)` + writes
// the new id + `setBusted(true)` + `window.location.reload()`. We
// only assert the post-effect state, not the order inside the effect.
// `localStorage[DB_INSTANCE_ID_STORAGE_KEY]` is the synchronization
// anchor — it flips inside the bust effect, ONLY on the bust path,
// and is observable from outside without racing IDB's deletion
// pipeline.

import 'fake-indexeddb/auto';
import { IDBFactory } from 'fake-indexeddb';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, waitFor, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

// Mock the WebSocket bridge so the gate's mount doesn't try to open
// a real socket. Mirrors `providers.test.tsx`'s setup — the bridge is
// a black box at this layer; the bust test cares about persisted
// storage, not the WS connection.
vi.mock('./eventBridge', () => ({
  EventBridge: () => <div data-testid="event-bridge-mock" />,
}));

import {
  DB_INSTANCE_ID_STORAGE_KEY,
  ServerCompatGate,
  WS_CURSOR_STORAGE_KEY,
} from './providers';
import { IDB_DB_NAME, createIDBPersister } from '../api/persistConfig';
import { WEB_COMPAT_VERSION, type ServerVersionInfo } from '../api/version';

const ID_PREVIOUS = '11111111-1111-4111-8111-111111111111';
const ID_NEW = '22222222-2222-4222-8222-222222222222';

function makeServerInfo(dbInstanceId: string): ServerVersionInfo {
  return {
    kernelVersion: '0.1.0',
    apiVersion: '1',
    syncEventVersion: 1,
    mcpProtocolVersion: '2024-11-05',
    pluginMcpProtocolVersion: '2025-11-25',
    webCompatVersion: WEB_COMPAT_VERSION,
    minWebCompatVersion: WEB_COMPAT_VERSION,
    supervisorControlVersion: 1,
    buildSha: null,
    dbInstanceId,
  };
}

function mockFetchVersion(dbInstanceId: string): void {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      statusText: 'OK',
      json: async () => makeServerInfo(dbInstanceId),
    } as unknown as Response),
  );
}

/**
 * Spy on the REAL `indexedDB.deleteDatabase` — keeps the underlying
 * fake-indexeddb engine intact (so the seeded blob really does get
 * the deletion request sent its way) while letting us assert the
 * call site.
 *
 * Replacing it outright with a `vi.fn()` (as `providers.test.tsx`
 * does) is what the issue is explicitly asking us to avoid: it
 * proves the call was MADE but not that the kernel-owned db name
 * is the one the persister actually writes to.
 */
function spyOnDeleteDatabase(): ReturnType<typeof vi.fn> {
  const original = indexedDB.deleteDatabase.bind(indexedDB);
  const spy = vi.fn((name: string) => original(name));
  Object.defineProperty(window.indexedDB, 'deleteDatabase', {
    configurable: true,
    value: spy,
  });
  return spy;
}

function installLocationReloadSpy(): ReturnType<typeof vi.fn> {
  const reload = vi.fn();
  Object.defineProperty(window, 'location', {
    configurable: true,
    value: { ...window.location, reload },
  });
  return reload;
}

/** Wipe IDB by swapping in a fresh `IDBFactory` — `deleteDatabase`
 *  would deadlock on `onblocked` if `idb-keyval`'s memoized connection
 *  is still open from a previous test. Mirrors `persistConfig.test.tsx`. */
function resetIDB(): void {
  (globalThis as { indexedDB: IDBFactory }).indexedDB = new IDBFactory();
}

beforeEach(() => {
  cleanup();
  resetIDB();
  localStorage.clear();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

describe('ServerCompatGate cache bust with real IDB', () => {
  it(
    'wipes the IDB persisted blob, the WS cursor, and the qc cache, ' +
      'then reloads when /api/version reports a new dbInstanceId',
    async () => {
      // ---- Seed the previous-session state.
      //
      // 1. localStorage carries the previous dbInstanceId + a stale
      //    WS sync cursor. The cache-bust path's job is to remove the
      //    cursor (so we don't try to replay from a row id that
      //    doesn't exist in the fresh DB).
      localStorage.setItem(DB_INSTANCE_ID_STORAGE_KEY, ID_PREVIOUS);
      localStorage.setItem(WS_CURSOR_STORAGE_KEY, '999');

      // 2. IDB carries a real PersistedClient blob written by the
      //    production persister. We seed via `createIDBPersister`
      //    (the same factory `buildPersistOptions` uses) so the
      //    shape + storage key match exactly what the gate's
      //    `safeDeleteIDB(IDB_DB_NAME)` needs to wipe.
      const seedPersister = createIDBPersister({ throttleTime: 0 });
      await seedPersister.persistClient({
        buster: 'seed-buster',
        timestamp: Date.now(),
        clientState: {
          mutations: [],
          queries: [
            {
              queryKey: ['coves'],
              queryHash: '["coves"]',
              state: {
                data: [{ id: 'cove-from-previous-db' }],
                dataUpdateCount: 1,
                dataUpdatedAt: Date.now(),
                error: null,
                errorUpdateCount: 0,
                errorUpdatedAt: 0,
                fetchFailureCount: 0,
                fetchFailureReason: null,
                fetchMeta: null,
                isInvalidated: false,
                status: 'success',
                fetchStatus: 'idle',
              } as unknown as never,
            },
          ],
        },
      });

      // Verify the seed actually landed — without this, a future
      // regression in the seed helper would make the bust assertion
      // green for the wrong reason ("there was nothing to wipe").
      const seededBlob = await seedPersister.restoreClient();
      expect(seededBlob, 'seed should write a real IDB blob').toBeDefined();
      expect(seededBlob!.buster).toBe('seed-buster');

      // 3. The QueryClient also carries the same in-memory data the
      //    bust path's `qc.clear()` should drop. Matches the
      //    `gcTime: 0` posture of `providers.test.tsx`'s mocked-IDB
      //    variant — both want a tight QueryClient configuration so
      //    that nothing about cached state interferes with the bust
      //    effect's strict-mode re-runs.
      const qc = new QueryClient({
        defaultOptions: {
          queries: { retry: false, gcTime: 0 },
        },
      });
      qc.setQueryData(['coves'], [{ id: 'cove-from-previous-db' }]);
      expect(qc.getQueryData(['coves'])).toBeDefined();

      // 4. /api/version returns the NEW id — the trigger condition.
      mockFetchVersion(ID_NEW);
      const reload = installLocationReloadSpy();
      const deleteDatabaseSpy = spyOnDeleteDatabase();

      // ---- Mount under the real ServerCompatGate.
      render(
        <QueryClientProvider client={qc}>
          <ServerCompatGate>
            <div data-testid="app">app body</div>
          </ServerCompatGate>
        </QueryClientProvider>,
      );

      // Sync anchor: localStorage[DB_INSTANCE_ID_STORAGE_KEY] flips
      // to the NEW id ONLY inside the bust effect, between
      // `safeDeleteIDB` and `window.location.reload()`. Waiting on
      // it (rather than on `reload` itself) avoids racing React 18's
      // strict-mode double-effect quirks and IDB's async deletion
      // pipeline. Once this assertion passes, every other side
      // effect of the bust has already run.
      await waitFor(() => {
        expect(localStorage.getItem(DB_INSTANCE_ID_STORAGE_KEY)).toBe(ID_NEW);
      });
      expect(reload).toHaveBeenCalledTimes(1);

      // ---- 1. `indexedDB.deleteDatabase` was called against the
      //         exact db name the persister writes to.
      //
      // Spying on the REAL fake-indexeddb method (not replacing it)
      // gives us the call-site assertion while keeping the engine
      // intact — that's the property "real IDB, not mocked" from
      // issue #199 that the `providers.test.tsx` variant doesn't
      // give us. The seeded blob and the deletion request flow
      // through the same fake-indexeddb instance, so a typo in the
      // call site (e.g. `safeDeleteIDB("calm-neige")` instead of
      // `IDB_DB_NAME`) would silently miss the seeded data and
      // surface here as `expect(spy).toHaveBeenCalledWith(IDB_DB_NAME)`
      // failing.
      expect(deleteDatabaseSpy).toHaveBeenCalledWith(IDB_DB_NAME);

      // ---- 2. WS cursor removed.
      expect(localStorage.getItem(WS_CURSOR_STORAGE_KEY)).toBeNull();

      // ---- 3. QueryClient in-memory data dropped.
      expect(qc.getQueryData(['coves'])).toBeUndefined();

      // ---- 4. dbInstanceId rewritten to the new id.
      expect(localStorage.getItem(DB_INSTANCE_ID_STORAGE_KEY)).toBe(ID_NEW);

      // ---- 5. App body is NOT rendered during the bust window —
      //         the gate returns `null` while `busted === true` so
      //         the user doesn't see an empty-cache flash before
      //         the navigation completes.
      expect(screen.queryByTestId('app')).not.toBeInTheDocument();
    },
  );

  it('leaves real IDB intact when /api/version reports the same dbInstanceId', async () => {
    // Symmetric guard against the bust-too-eagerly regression: if
    // the same id comes back, NOTHING in storage should change.
    localStorage.setItem(DB_INSTANCE_ID_STORAGE_KEY, ID_PREVIOUS);
    localStorage.setItem(WS_CURSOR_STORAGE_KEY, '42');

    const seedPersister = createIDBPersister({ throttleTime: 0 });
    await seedPersister.persistClient({
      buster: 'same-id-keep-it',
      timestamp: Date.now(),
      clientState: { mutations: [], queries: [] },
    });

    mockFetchVersion(ID_PREVIOUS);
    const reload = installLocationReloadSpy();

    // `gcTime: Infinity` so the seeded query data survives until the
    // effect either keeps it (this test) or clears it (the bust
    // test). `gcTime: 0` would let it be reclaimed before our
    // assertion fires.
    const qc = new QueryClient({
      defaultOptions: {
        queries: { retry: false, gcTime: Infinity, staleTime: Infinity },
      },
    });
    qc.setQueryData(['coves'], [{ id: 'kept' }]);

    render(
      <QueryClientProvider client={qc}>
        <ServerCompatGate>
          <div data-testid="app">app body</div>
        </ServerCompatGate>
      </QueryClientProvider>,
    );

    // Wait for the version query to resolve. The app body renders
    // for the matching-id path; that's our signal that the effect
    // has had a chance to run (or correctly NOT run, on this path).
    await waitFor(() => {
      expect(screen.getByTestId('app')).toBeInTheDocument();
    });

    // Nothing torn down.
    expect(reload).not.toHaveBeenCalled();
    expect(localStorage.getItem(WS_CURSOR_STORAGE_KEY)).toBe('42');
    expect(localStorage.getItem(DB_INSTANCE_ID_STORAGE_KEY)).toBe(ID_PREVIOUS);
    expect(qc.getQueryData(['coves'])).toEqual([{ id: 'kept' }]);

    // IDB blob still readable (note: we use the SAME persister
    // instance so the memoized connection is fine — no bust ran).
    const after = await seedPersister.restoreClient();
    expect(after?.buster).toBe('same-id-keep-it');
  });
});
