// Regression pin for `useTodayTerminal`'s track-create body (#1147 S3).
//
// The hook used to mint the Today track with `cwd: '/'` + `attach_folder: false`.
// Since #1131/S2 an *omitted* `cwd` is the managed branch (the kernel allocates,
// creates and `git init`s `<workspace-root>/<area>/<track>`), while an *explicit*
// `cwd` means "attach this existing repository" and is validated as absolute +
// existing + inside a Git work tree. `/` is not a Git work tree, so the old body
// was both the live source of the #1147 symptom (workers dying in
// `git_repo_root_for_track_cwd` with a bare `spawn-failed`) and, after S3, an
// outright 400 on the Today bootstrap.
//
// So the thing worth pinning is not a value but an *absence*: the track-create
// request body must carry neither key. We assert on the JSON that actually goes
// on the wire (global `fetch` is stubbed, `api/calm` is the real module) rather
// than on hook state, because "what we send" is the contract that broke.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

import { useTodayTerminal } from './useTodayTerminal';

interface Call {
  method: string;
  path: string;
  body: unknown;
}

let calls: Call[] = [];

function json(status: number, payload: unknown): Response {
  return {
    ok: status < 400,
    status,
    statusText: String(status),
    json: async () => payload,
  } as unknown as Response;
}

/** Minimal stub of the kernel routes `useTodayTerminal` walks on bootstrap. */
function installFetchStub() {
  const fetchMock = vi.fn(async (path: string, init?: RequestInit) => {
    const method = init?.method ?? 'GET';
    const body =
      typeof init?.body === 'string' ? JSON.parse(init.body) : undefined;
    calls.push({ method, path, body });

    if (method === 'POST' && path === '/api/areas/system') {
      return json(200, { id: 'area-system', title: 'system', kind: 'system' });
    }
    if (method === 'GET' && path === '/api/areas/area-system/tracks') {
      // No Today track yet → the hook takes the mint branch under test.
      return json(200, []);
    }
    if (method === 'POST' && path === '/api/tracks') {
      return json(201, { id: 'track-today', area_id: 'area-system', title: 'Today' });
    }
    if (method === 'GET' && path === '/api/tracks/track-today') {
      return json(200, { id: 'track-today', cards: [] });
    }
    if (method === 'POST' && path === '/api/tracks/track-today/terminal-cards') {
      return json(201, {
        id: 'card-today',
        kind: 'terminal',
        payload: { terminal_id: 'term-today' },
      });
    }
    return json(404, { code: 'not_found', error: `unstubbed ${method} ${path}` });
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

function wrapper() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

beforeEach(() => {
  calls = [];
  localStorage.clear();
  installFetchStub();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('useTodayTerminal track-create body', () => {
  it('omits `cwd` and `attach_folder` entirely when minting the Today track', async () => {
    const { result } = renderHook(() => useTodayTerminal(), { wrapper: wrapper() });

    await waitFor(() => {
      expect(result.current.today).not.toBeNull();
    });
    expect(result.current.error).toBeNull();

    const createTrack = calls.filter(
      (c) => c.method === 'POST' && c.path === '/api/tracks',
    );
    expect(createTrack).toHaveLength(1);
    const body = createTrack[0].body as Record<string, unknown>;

    // Key ABSENCE, not `=== undefined`: `cwd: undefined` would be dropped by
    // `JSON.stringify` here but a future refactor could reintroduce a real
    // value, and `toHaveProperty` is the assertion that notices either way.
    expect(body).not.toHaveProperty('cwd');
    expect(body).not.toHaveProperty('attach_folder');

    // The keys that must still be there, so "omit everything" can't pass.
    expect(body.area_id).toBe('area-system');
    expect(body.title).toBe('Today');
  });
});
