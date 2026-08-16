// @vitest-environment jsdom
// Invariants owned by the route tree.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter, createRouteTree, pendingConversationIds } from './public.tsx';
import { pathFor, routeParamFromPath } from './navigation.ts';

const COVE = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

/** The coves list is non-empty on purpose: with zero coves a fan-out in the
 *  loader would be unobservable and the INV-APP-084 assertion vacuous. */
function recordingTransport(): { transport: ApiTransportPort; paths: string[] } {
  const paths: string[] = [];
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      paths.push(request.path);
      return Promise.resolve({
        status: 200,
        statusText: 'OK',
        body: request.path === '/api/coves' ? [COVE] : [],
      });
    },
  };
  return { transport, paths };
}

function routeByPath(tree: ReturnType<typeof createRouteTree>, path: string) {
  // `path` / `id` are only populated once a router initialises the tree; the
  // authored value lives on `options`.
  const children = tree.children as { options: { path?: string; loader?: () => unknown } }[];
  const match = children.find((child) => child.options.path === path);
  if (!match) throw new Error(`no route registered for ${path}`);
  return match;
}

describe('INV-APP-084 the index loader primes coves and nothing else', () => {
  it('issues exactly one request — the coves list — when the loader runs', async () => {
    const { transport, paths } = recordingTransport();
    const tree = createRouteTree({ transport, unauthorized, client: new QueryClient(), onSignOut: () => undefined });
    await routeByPath(tree, '/').options.loader?.();
    expect(paths).toEqual(['/api/coves']);
  });

  it('leaves the cove → waves fan-out off the loader so one slow cove cannot block the commit', async () => {
    const { transport, paths } = recordingTransport();
    const tree = createRouteTree({ transport, unauthorized, client: new QueryClient(), onSignOut: () => undefined });
    await routeByPath(tree, '/').options.loader?.();
    expect(paths.filter((path) => path.endsWith('/waves'))).toEqual([]);
  });

  it('gives the other routes no loader at all', () => {
    const { transport } = recordingTransport();
    const tree = createRouteTree({ transport, unauthorized, client: new QueryClient(), onSignOut: () => undefined });
    for (const path of ['/cove/$coveId', '/wave/$waveId', '/settings']) {
      expect(routeByPath(tree, path).options.loader).toBeUndefined();
    }
  });
});

describe('route registration', () => {
  it('renders the index route at the deployed /next/ basepath', async () => {
    const { transport } = recordingTransport();
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const router = createAppRouter({ transport, unauthorized, client, onSignOut: () => undefined });
    router.update({ history: createMemoryHistory({ initialEntries: ['/next/'] }) });

    render(
      <QueryClientProvider client={client}>
        <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
          <RouterProvider router={router} />
        </ThemeProvider>
      </QueryClientProvider>,
    );

    expect(await screen.findByRole('complementary')).toBeTruthy();
    expect(screen.getByLabelText('Today terminal')).toBeTruthy();
  });

  it('registers the four product routes', () => {
    const { transport } = recordingTransport();
    const tree = createRouteTree({ transport, unauthorized, client: new QueryClient(), onSignOut: () => undefined });
    const paths = (tree.children as { options: { path?: string } }[]).map((child) => child.options.path);
    expect(paths).toEqual(['/', '/cove/$coveId', '/wave/$waveId', '/settings']);
  });

  it('builds every navigation target as a path the tree can match', () => {
    expect(pathFor({ name: 'today' })).toBe('/');
    expect(pathFor({ name: 'cove', coveId: 'c1' })).toBe('/cove/c1');
    expect(pathFor({ name: 'wave', waveId: 'w1' })).toBe('/wave/w1');
    expect(pathFor({ name: 'settings' })).toBe('/settings');
  });
});

describe('route parameter codec', () => {
  it('percent-encodes ids on output and decodes them on input', () => {
    expect(pathFor({ name: 'wave', waveId: 'a/b %' })).toBe('/wave/a%2Fb%20%25');
    expect(routeParamFromPath('/wave/a%2Fb%20%25', '/wave/')).toBe('a/b %');
  });

  it('treats malformed percent escapes as an unmatched parameter instead of throwing', () => {
    expect(routeParamFromPath('/wave/%', '/wave/')).toBeUndefined();
  });
});

afterEach(() => { cleanup(); vi.useRealTimers(); });

describe('conversation pending accounting', () => {
  const conversation = {
    id: 'c1', waveId: 'w1', waveTitle: 'Wave', title: null, kind: 'shared-spec' as const,
    state: 'idle' as const, updatedAt: 0, turns: 0,
  };

  it('bridges the user-visible Working state from input submission to the real harness phase', () => {
    expect(pendingConversationIds(conversation, false, true).has('c1')).toBe(true);
    expect(pendingConversationIds(conversation, true, false).has('c1')).toBe(true);
    expect(pendingConversationIds(conversation, false, false).has('c1')).toBe(false);
  });

  /* Pending is what *this* tab is doing, so it cannot depend on the session
     state the server last reported — including its absence on a chat row. */
  it('accounts a conversation with no reported session the same way', () => {
    const stateless = { ...conversation, kind: 'shared-chat' as const, state: null };
    expect(pendingConversationIds(stateless, false, true).has('c1')).toBe(true);
    expect(pendingConversationIds(stateless, false, false).has('c1')).toBe(false);
  });
});
