// @vitest-environment jsdom
// Invariants owned by the route tree.
import { QueryClient } from '@tanstack/react-query';
import { describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createRouteTree } from './public.tsx';
import { pathFor } from './navigation.ts';

const COVE = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };

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
    const tree = createRouteTree({ transport, client: new QueryClient() });
    await routeByPath(tree, '/').options.loader?.();
    expect(paths).toEqual(['/api/coves']);
  });

  it('leaves the cove → waves fan-out off the loader so one slow cove cannot block the commit', async () => {
    const { transport, paths } = recordingTransport();
    const tree = createRouteTree({ transport, client: new QueryClient() });
    await routeByPath(tree, '/').options.loader?.();
    expect(paths.filter((path) => path.endsWith('/waves'))).toEqual([]);
  });

  it('gives the other routes no loader at all', () => {
    const { transport } = recordingTransport();
    const tree = createRouteTree({ transport, client: new QueryClient() });
    for (const path of ['/cove/$coveId', '/wave/$waveId', '/settings']) {
      expect(routeByPath(tree, path).options.loader).toBeUndefined();
    }
  });
});

describe('route registration', () => {
  it('registers the four product routes', () => {
    const { transport } = recordingTransport();
    const tree = createRouteTree({ transport, client: new QueryClient() });
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
