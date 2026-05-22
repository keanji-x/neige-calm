/**
 * Auth wire (issue #189). Single-user owner model — calm-server signs the
 * session cookie via `POST /api/auth/login` and validates it on every
 * subsequent REST + WS request.
 *
 * `credentials: 'include'` on every call so the `calm-session` cookie
 * survives across origins under the Vite dev proxy (cookies are httpOnly
 * + SameSite=Strict on the server side).
 */

/** Shape returned by `/api/auth/whoami` and `/api/auth/login` (success). */
export interface WhoamiResponse {
  userId: string;
  displayName: string;
  role: string;
  sessionId: string;
}

/**
 * Probe the current session. Returns the whoami payload on 200, `null`
 * on 401 (caller treats `null` as "show LoginPage"), and throws on any
 * other transport error (caller renders the unknown-error state).
 *
 * Keeps the transport surface tight: we INTENTIONALLY do not throw on
 * 401 because that's the expected "no session" response and the
 * SessionProvider needs to discriminate it from a real network failure.
 */
export async function whoami(): Promise<WhoamiResponse | null> {
  const res = await fetch('/api/auth/whoami', {
    credentials: 'include',
  });
  if (res.status === 401) return null;
  if (!res.ok) {
    throw new Error(`whoami failed: HTTP ${res.status}`);
  }
  return (await res.json()) as WhoamiResponse;
}

/**
 * POST `/api/auth/login` with the configured owner credentials. On
 * success the server sets the `calm-session` cookie and returns the
 * whoami payload; we return it to the caller for an immediate render
 * without a second round-trip.
 *
 * Returns `null` on 401 (wrong credentials) so the LoginPage can show
 * a tight error message; other errors propagate.
 */
export async function login(
  username: string,
  password: string,
): Promise<WhoamiResponse | null> {
  const res = await fetch('/api/auth/login', {
    method: 'POST',
    credentials: 'include',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
  });
  if (res.status === 401) return null;
  if (!res.ok) {
    throw new Error(`login failed: HTTP ${res.status}`);
  }
  return (await res.json()) as WhoamiResponse;
}

/**
 * POST `/api/auth/logout` — server drops the session id + clears the
 * cookie. We surface the boolean so callers can branch on transport
 * failure, but the recommended flow is to fire-and-forget then trigger
 * the local unauthorized cleanup regardless (the cache wipe + reload is
 * the load-bearing user-visible state change).
 */
export async function logout(): Promise<boolean> {
  try {
    const res = await fetch('/api/auth/logout', {
      method: 'POST',
      credentials: 'include',
    });
    return res.ok;
  } catch {
    return false;
  }
}
