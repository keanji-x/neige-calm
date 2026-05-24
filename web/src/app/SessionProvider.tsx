/**
 * SessionProvider — global session gate (issue #189).
 *
 * Wraps the `RouterProvider`. On mount it calls `/api/auth/whoami`:
 *
 *   * 200 → renders `children` (the router). The user is authenticated;
 *           route loaders are now free to hit business APIs.
 *   * 401 → renders `<LoginPage />`. The router NEVER mounts in this
 *           branch, which is the load-bearing invariant: route loaders
 *           must not race the auth gate. A logged-out tab landing on
 *           `/coves/c-1` must not stamp a 401 onto the first paint.
 *   * other → renders a minimal loading / error stub. We don't bounce to
 *           LoginPage on a transient network blip — refresh is the
 *           recommended recovery path.
 *
 * When any API call observes a 401 (REST via `calm.ts`, WS via the
 * upgrade probe in `events.ts`), it fires the global `onUnauthorized`
 * channel. SessionProvider's listener clears every cache + cursor +
 * resets to LoginPage; the user logs in again, whoami succeeds, and the
 * router remounts.
 *
 * `logout()` triggers the same cleanup as a 401 — the only difference
 * is that the server-side cookie also gets cleared.
 *
 * Lives in `web/src/app/` next to `providers.tsx` because it sits in the
 * same provider stack; the gate is mounted by `main.tsx` immediately
 * inside `AppProviders`.
 */

import { createContext, useContext, useEffect } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useState } from '../shared/state';
import { whoami, type WhoamiResponse } from '../api/auth';
import { onUnauthorized } from '../api/onUnauthorized';
import { LoginPage } from '../LoginPage';
import { IDB_DB_NAME } from '../api/persistConfig';

// Context carries the whoami payload to descendants of the authed branch.
// Null only outside an authed render — the `useSession` hook below throws
// in that case so consumers can rely on the non-null value.
//
// Exported so vitest integration tests that mount session-aware leaves
// (e.g. `<Sidebar>`'s `UserMenu`) can wrap their render tree in a stub
// provider without standing up the full whoami probe. Production code
// should keep using `<SessionProvider>` / `useSession()` — direct
// `SessionContext.Provider` usage is a test-only seam.
export const SessionContext = createContext<WhoamiResponse | null>(null);

/**
 * Read the current authed session. Must be called inside an authed
 * SessionProvider render (which is the only branch that mounts `children`),
 * otherwise it throws — null sessions don't reach UI that uses this hook.
 */
export function useSession(): WhoamiResponse {
  const ctx = useContext(SessionContext);
  if (!ctx) {
    throw new Error('useSession must be used inside an authed SessionProvider');
  }
  return ctx;
}

/** Same localStorage key the WS event stream uses for its `since` cursor.
 *  Duplicated here (not imported) to keep this module's import graph free
 *  of the WS stream itself — see comment in `providers.tsx`. */
const WS_CURSOR_STORAGE_KEY = 'calm:sync:cursor';

/**
 * Tri-state lifecycle. `unknown` is the initial mount tick before the
 * first whoami resolves; the gate renders `null` then so we don't flash
 * a LoginPage or the dashboard before we know which one is correct.
 */
type SessionState =
  | { kind: 'unknown' }
  | { kind: 'authed'; whoami: WhoamiResponse }
  | { kind: 'unauthed' }
  | { kind: 'error'; message: string };

export interface SessionProviderProps {
  children: React.ReactNode;
}

export function SessionProvider({ children }: SessionProviderProps) {
  const [session, setSession] = useState<SessionState>({ kind: 'unknown' });
  const qc = useQueryClient();

  // Initial whoami probe — runs once per mount. Cancelled via a flag if
  // the component unmounts before the probe resolves (StrictMode double-
  // mounts in dev would otherwise log a "set state on unmounted").
  useEffect(() => {
    let cancelled = false;
    whoami()
      .then((w) => {
        if (cancelled) return;
        if (w) {
          setSession({ kind: 'authed', whoami: w });
        } else {
          setSession({ kind: 'unauthed' });
        }
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setSession({
          kind: 'error',
          message: err instanceof Error ? err.message : 'whoami failed',
        });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Global 401 listener — any API call that observes a 401 fires
  // `fireUnauthorized()`, which lands here. We wipe every persisted
  // client-side artifact then flip back to LoginPage.
  //
  // `qc` lands in the dependency array so the listener gets re-registered
  // if React Query Provider is ever swapped (it isn't today, but
  // exhaustive deps keeps the lint clean and the contract honest).
  useEffect(() => {
    return onUnauthorized(() => {
      try {
        qc.clear();
      } catch {
        /* unreachable in practice; defensive */
      }
      // Drop the WS event cursor — a stale `since` from the previous
      // session would otherwise replay events into a logged-out tab if
      // the next user logged in immediately. The cursor is single-user
      // anyway; dropping is the right behavior.
      try {
        localStorage.removeItem(WS_CURSOR_STORAGE_KEY);
      } catch {
        /* private mode / quota — degrade silently */
      }
      // Wipe the React Query persistent cache (IndexedDB). Same
      // reasoning as ServerCompatGate: dead row ids must not paint.
      try {
        indexedDB.deleteDatabase(IDB_DB_NAME);
      } catch {
        /* IDB unavailable — degrade silently */
      }
      setSession({ kind: 'unauthed' });
    });
  }, [qc]);

  if (session.kind === 'unknown') {
    // First mount tick — whoami in flight. Render nothing rather than a
    // flash of LoginPage. The probe is cheap (single fetch); the user
    // typically sees this for ~50ms.
    return null;
  }
  if (session.kind === 'error') {
    // Transport error (network down, server crash). Show a tight
    // recovery hint; don't bounce to LoginPage because the user may
    // very well be logged in once the network comes back.
    return (
      <div className="login-page">
        <div className="login-card">
          <div className="login-eyebrow">Neige · Calm</div>
          <h1 className="login-title">Cannot reach server.</h1>
          <p className="login-hint">{session.message}</p>
          <button
            type="button"
            className="go"
            onClick={() => window.location.reload()}
            style={{ width: '100%', justifyContent: 'center', marginTop: 4 }}
          >
            Retry
          </button>
        </div>
      </div>
    );
  }
  if (session.kind === 'unauthed') {
    return <LoginPage />;
  }
  return (
    <SessionContext.Provider value={session.whoami}>
      {children}
    </SessionContext.Provider>
  );
}
