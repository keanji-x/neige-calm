// Tests for `web/src/api/auth.ts` (issue #189).
//
// Verifies the wire shape end-to-end via a stubbed `fetch`:
//   * whoami: 401 → null; 200 → parsed body.
//   * login: passes {username, password} JSON; 401 → null; 200 → body.
//   * logout: POSTs to the right path with credentials: include.
//
// Hermetic — no network, only `vi.stubGlobal('fetch', ...)`.

import { describe, it, expect, vi, afterEach } from 'vitest';
import { login, logout, whoami } from './auth';

function stubFetch(handler: (input: RequestInfo, init?: RequestInit) => Response | Promise<Response>) {
  vi.stubGlobal('fetch', vi.fn(handler));
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('whoami', () => {
  it('returns null on 401', async () => {
    stubFetch(() => ({
      ok: false,
      status: 401,
      statusText: 'Unauthorized',
      json: async () => ({ error: 'unauthorized', code: 'unauthorized' }),
    } as unknown as Response));
    expect(await whoami()).toBeNull();
  });

  it('returns the parsed owner whoami on 200', async () => {
    stubFetch(() => ({
      ok: true,
      status: 200,
      statusText: 'OK',
      json: async () => ({
        userId: 'local-owner',
        displayName: 'Owner',
        role: 'owner',
        sessionId: 'sess-1',
      }),
    } as unknown as Response));
    const w = await whoami();
    expect(w?.userId).toBe('local-owner');
    expect(w?.role).toBe('owner');
  });

  it('throws on non-401 transport failure', async () => {
    stubFetch(() => ({
      ok: false,
      status: 500,
      statusText: 'Server Error',
      json: async () => ({}),
    } as unknown as Response));
    await expect(whoami()).rejects.toThrow(/whoami failed/);
  });
});

describe('login', () => {
  it('posts {username, password} as JSON with credentials: include', async () => {
    // Typed signature on the mock so `fetchSpy.mock.calls[0]` infers a
    // `[RequestInfo, RequestInit | undefined]` tuple rather than the
    // default `unknown[]`.
    const fetchSpy = vi.fn(
      (_path: RequestInfo, _init?: RequestInit): Promise<Response> =>
        Promise.resolve({
          ok: true,
          status: 200,
          statusText: 'OK',
          json: async () => ({
            userId: 'local-owner',
            displayName: 'alice',
            role: 'owner',
            sessionId: 'sess-2',
          }),
        } as unknown as Response),
    );
    vi.stubGlobal('fetch', fetchSpy);

    const got = await login('alice', 'hunter2');
    expect(got?.displayName).toBe('alice');

    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [path, init] = fetchSpy.mock.calls[0];
    expect(path).toBe('/api/auth/login');
    expect(init?.method).toBe('POST');
    expect(init?.credentials).toBe('include');
    expect(JSON.parse(init?.body as string)).toEqual({
      username: 'alice',
      password: 'hunter2',
    });
  });

  it('returns null on 401', async () => {
    stubFetch(() => ({
      ok: false,
      status: 401,
      statusText: 'Unauthorized',
      json: async () => ({ error: 'unauthorized', code: 'unauthorized' }),
    } as unknown as Response));
    expect(await login('alice', 'wrong')).toBeNull();
  });
});

describe('logout', () => {
  it('POSTs /api/auth/logout with credentials: include', async () => {
    const fetchSpy = vi.fn(
      (_path: RequestInfo, _init?: RequestInit): Promise<Response> =>
        Promise.resolve({
          ok: true,
          status: 200,
          statusText: 'OK',
          json: async () => ({ ok: true }),
        } as unknown as Response),
    );
    vi.stubGlobal('fetch', fetchSpy);

    expect(await logout()).toBe(true);
    const [path, init] = fetchSpy.mock.calls[0];
    expect(path).toBe('/api/auth/logout');
    expect(init?.method).toBe('POST');
    expect(init?.credentials).toBe('include');
  });
});
