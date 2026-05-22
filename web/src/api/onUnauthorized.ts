/**
 * Global unauthorized callback (issue #189).
 *
 * Any API client that observes a 401 — `calm.ts` for REST, the WS bridge
 * for `/api/events` + `/api/terminals/*` — funnels through
 * [`fireUnauthorized`]. The `SessionProvider` registers a listener on
 * mount that:
 *   1. wipes the React Query in-memory + IDB-persisted cache,
 *   2. drops `calm:sync:cursor` from localStorage,
 *   3. tears down the WS connection,
 *   4. flips its own state back to "show LoginPage".
 *
 * The indirection lives in a leaf module to keep the import graph
 * acyclic: `calm.ts` doesn't need to know about React, the QueryClient,
 * or the SessionProvider — it just calls `fireUnauthorized()`. That
 * single seam means there's exactly one place to extend with new
 * cleanup steps (or to mock in tests).
 *
 * Multiple listeners are allowed but only the first one really matters
 * in practice — the SessionProvider is the canonical owner. Extra
 * listeners are useful for tests / dev assertions.
 */

type Listener = () => void;

const listeners = new Set<Listener>();

/**
 * Register a callback to fire on the next (and every future) 401. Returns
 * an unsubscribe function. The SessionProvider calls this once in its
 * `useEffect`.
 */
export function onUnauthorized(fn: Listener): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/**
 * Trigger every registered listener. Safe to call multiple times in
 * quick succession — listeners are responsible for being idempotent
 * (the SessionProvider's wipe is).
 *
 * Wrapped in `queueMicrotask` so a 401-throwing fetch unwinds cleanly
 * before listeners try to invalidate React Query state — otherwise the
 * caller's `.catch()` would race with `queryClient.clear()`.
 */
export function fireUnauthorized(): void {
  queueMicrotask(() => {
    for (const fn of listeners) {
      try {
        fn();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error('onUnauthorized listener threw', err);
      }
    }
  });
}

/**
 * Test-only: drop every listener. Vitest's module isolation usually
 * handles this for us (each test file gets a fresh import graph), but
 * tests that share a file and want to assert listener counts can use
 * this for a clean slate.
 */
export function _resetUnauthorizedListenersForTest(): void {
  listeners.clear();
}
