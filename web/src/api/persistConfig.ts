// TanStack Query persistence — IndexedDB-backed cache survival across reloads.
//
// What this gives us:
//   - On reload, the cached coves / waves / overlays paint immediately from
//     IndexedDB before any HTTP request goes out. React Query still refetches
//     in the background per the normal staleTime / refetchOnMount rules, so
//     fresh data eventually replaces the cached snapshot without anyone
//     noticing — but the user no longer stares at an empty UI during the
//     round-trip.
//   - Offline reloads keep showing the last-known-good state instead of an
//     empty grid.
//
// Why an allowlist (and not "persist everything"):
//   - Settings is short-lived UI form state; persisting it across builds is
//     more confusing than helpful.
//   - Anything new (transient debug queries, ephemeral lookups) shouldn't
//     accidentally bloat the IndexedDB cache. We enumerate only the keys we
//     know are safe to re-show stale on next paint. New keys must be opted
//     in explicitly here.
//
// Key shapes (kept in sync with `api/queries.ts` — see `queryKeys` there):
//   ['coves']                         — list of all coves
//   ['waves', coveId]                 — waves in a cove
//   ['wave', waveId]                  — wave detail (cards + overlays)
//   ['overlays', 'wave' | 'card']     — workspace-wide overlay snapshot
//   ['overlay', ...]                  — reserved future per-entity overlay key
//
// Buster strategy: cache schema + the web app's `package.json` version.
// Bump `PERSIST_CACHE_SCHEMA_VERSION` when the persisted shape changes; bump
// the package version when a release should invalidate old snapshots.

import { createAsyncStoragePersister } from '@tanstack/query-async-storage-persister';
import { defaultShouldDehydrateQuery, type Query } from '@tanstack/react-query';
import { createStore, get as idbGet, set as idbSet, del as idbDel } from 'idb-keyval';
import pkg from '../../package.json';

/** IndexedDB database name. Owned exclusively by the calm web app. */
export const IDB_DB_NAME = 'neige-calm';
/** Object store inside that DB. */
export const IDB_STORE_NAME = 'query-cache';
/** Key under which the serialized PersistedClient blob is written. The `v1`
 *  suffix lets us bump the on-disk layout independently of the package
 *  version buster (e.g. if we ever switch serializers). */
export const IDB_CACHE_KEY = 'tanstack-query-v1';

/** Cache lifetime — match the kernel's "the world might have moved on a lot"
 *  threshold. Stale-but-not-too-stale: a week-old cache is still useful as a
 *  paint-before-refetch hint, but anything older is mostly noise. */
export const PERSIST_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

/** Manual schema lever for cache-format or API-contract changes that should
 *  evict all persisted Query snapshots, independent of package release
 *  versioning discipline. */
export const PERSIST_CACHE_SCHEMA_VERSION = 'query-cache-v1';

/** Build-time buster derived from cache schema + `web/package.json` version.
 *  Exported so the provider and tests read the same source of truth. */
export const PERSIST_BUSTER: string = `${PERSIST_CACHE_SCHEMA_VERSION}:${pkg.version}`;

/**
 * Build an IndexedDB-backed persister.
 *
 * The default `createStore` from idb-keyval namespaces a DB + store; the
 * three `idb-keyval` helpers (get/set/del) accept it as a second arg. We
 * wrap them into the `AsyncStorage` shape TanStack Query expects.
 *
 * `createStore` is called lazily (inside the factory) so unit tests that
 * mock IndexedDB before importing this module aren't racing the module-load
 * side effect of opening a real DB.
 *
 * `throttleTime` (default 1s) is the debounce window between cache mutation
 * and the resulting IndexedDB write — short enough that a quick reload
 * still gets the freshest data, long enough that mutation bursts don't
 * thrash IDB. Tests pass `0` to flush deterministically.
 */
export function createIDBPersister(opts: { throttleTime?: number } = {}) {
  const store = createStore(IDB_DB_NAME, IDB_STORE_NAME);
  return createAsyncStoragePersister({
    storage: {
      getItem: async (key) => {
        const value = await idbGet<string>(key, store);
        return value ?? null;
      },
      setItem: async (key, value) => {
        await idbSet(key, value, store);
      },
      removeItem: async (key) => {
        await idbDel(key, store);
      },
    },
    key: IDB_CACHE_KEY,
    ...(opts.throttleTime !== undefined ? { throttleTime: opts.throttleTime } : {}),
  });
}

/**
 * Allowlist predicate fed to TanStack Query's `dehydrateOptions`. Returns
 * `true` if a query's key matches one of the shapes we want to persist.
 *
 * Matched shapes (in sync with `queryKeys` in `api/queries.ts`):
 *   ['coves']
 *   ['waves', *]
 *   ['wave', *]
 *   ['overlays', *]
 *   ['overlay', *]
 *
 * Everything else (e.g. `['settings']`, future ad-hoc keys) is intentionally
 * dropped — see the file-level comment for the rationale.
 */
export function isPersistableQueryKey(query: Pick<Query, 'queryKey'>): boolean {
  const key = query.queryKey;
  if (!Array.isArray(key) || key.length === 0) return false;
  const root = key[0];
  if (root === 'coves') return key.length === 1;
  if (root === 'waves' || root === 'wave') return key.length >= 2;
  if (root === 'overlays' || root === 'overlay') return key.length >= 2;
  return false;
}

export function shouldPersistQuery(query: Query): boolean {
  return defaultShouldDehydrateQuery(query) && isPersistableQueryKey(query);
}

/**
 * The full `persistOptions` blob to hand to `<PersistQueryClientProvider>`.
 * Importing this from `providers.tsx` keeps that file focused on React tree
 * wiring; all the persistence policy lives here.
 */
export function buildPersistOptions() {
  return {
    persister: createIDBPersister(),
    maxAge: PERSIST_MAX_AGE_MS,
    buster: PERSIST_BUSTER,
    dehydrateOptions: {
      shouldDehydrateQuery: shouldPersistQuery,
    },
  };
}
