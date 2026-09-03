// @vitest-environment jsdom
//
// Settings › Plugins › configuration, wired to the real thing.
//
// The pane's own tests drive it through its props, which cannot see the two
// claims that only exist once the app is assembled:
//
//   1. **the request that leaves the browser carries only the edited keys**
//      (#1284 §2.2.5). The pane can be shown to *build* such a patch; that it
//      reaches the kernel that way is a fact about `usePluginConfigMutations`,
//      the operation descriptor and the transport, and this file reads it off
//      the recorded request.
//   2. **a restart's verdict comes from re-reading the plugin, not from the
//      status code** (§2.4). Here the reload answers 500 with a body that says
//      nothing useful, and the plugin is `unavailable` with a `last_error`
//      afterwards. A host that trusted the status code cannot produce that
//      sentence, so the assertion is only satisfiable by the extra read.
//
// So this runs the router, the query client and the transport the application
// uses, and stubs nothing but the network.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const CONFIG_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    token: { type: 'string' },
    base_url: { type: 'string', default: 'https://api.github.com' },
    verbose: { type: 'boolean', default: true },
  },
};

const ROW = {
  id: 'git-forge',
  version: '0.1.0',
  enabled: true,
  state: 'running',
  manifest_name: 'Git forge',
  has_config: true,
};

function detailBody(overrides: Record<string, unknown> = {}): unknown {
  return {
    id: 'git-forge',
    version: '0.1.0',
    enabled: true,
    state: 'running',
    manifest: { id: 'git-forge', display_name: 'Git forge' },
    config_schema: CONFIG_SCHEMA,
    user_config: { token: 'stored' },
    effective_config: { token: 'stored', base_url: 'https://api.github.com', verbose: true },
    installed_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

type Reply = (request: ApiRequest, calls: readonly ApiRequest[]) => ApiTransportResponse;

function renderPlugins(reply: Reply) {
  const calls: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request): Promise<ApiTransportResponse> {
      calls.push(request);
      return Promise.resolve(reply(request, calls));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined,
  });
  router.update({ history: createMemoryHistory({ initialEntries: ['/settings/plugins'] }) });
  render(
    <QueryClientProvider client={client}>
      <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return calls;
}

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

/** Everything the rest of the app reads while Settings is open. */
function baseline(request: ApiRequest): ApiTransportResponse | null {
  if (request.path === '/api/plugins') return ok([ROW]);
  if (request.path === '/api/settings') return ok({ settings: {} });
  if (request.path === '/api/areas' || request.path === '/api/track-templates') return ok([]);
  return null;
}

describe('Settings › Plugins › configuration, end to end', () => {
  it('sends only the edited key, then restarts, then confirms', async () => {
    const calls = renderPlugins((request) => {
      const shared = baseline(request);
      if (shared !== null) return shared;
      if (request.path === '/api/plugins/git-forge') return ok(detailBody());
      if (request.path === '/api/plugins/git-forge/config') {
        return ok(detailBody({ user_config: { token: 'stored', base_url: 'https://forge.internal' } }));
      }
      if (request.path === '/api/plugins/git-forge/reload') return ok(detailBody({ state: 'running' }));
      return ok([]);
    });

    await userEvent.click(await screen.findByRole('button', { name: 'Configure Git forge' }));
    const field = await screen.findByLabelText('base_url');
    await userEvent.type(field, 'https://forge.internal');
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));

    await screen.findByText('Configuration saved and the plugin restarted with it.');

    const patch = calls.find((call) => call.method === 'PATCH');
    expect(patch?.path).toBe('/api/plugins/git-forge/config');
    /*
     * The whole of §2.2.5 in one assertion, read off the wire: `token` was
     * stored and untouched, `base_url` and `verbose` have manifest defaults and
     * only `base_url` was typed into. A form that posted its effective state
     * would send three keys here and freeze two of today's defaults into the
     * row forever.
     */
    expect(patch?.body).toEqual({ base_url: 'https://forge.internal' });
    // And the restart followed the write, rather than replacing it.
    const order = calls.filter((call) => call.method !== 'GET').map((call) => call.method);
    expect(order).toEqual(['PATCH', 'POST']);
  });

  it('reads the plugin back after a failed restart and reports what it found', async () => {
    const reason = 'mcp-http: connect to https://forge.internal failed: connection refused';
    const calls = renderPlugins((request) => {
      const shared = baseline(request);
      if (shared !== null) return shared;
      if (request.path === '/api/plugins/git-forge') {
        /* Before the reload the plugin is fine; after it, it is `unavailable`
           with the reason. The switch is keyed on whether the reload has been
           attempted, so the second read is the only way to this sentence. */
        const restarted = calls.some((call) => call.path === '/api/plugins/git-forge/reload');
        return ok(restarted
          ? detailBody({ state: 'unavailable', last_error: reason })
          : detailBody());
      }
      if (request.path === '/api/plugins/git-forge/config') return ok(detailBody());
      if (request.path === '/api/plugins/git-forge/reload') {
        return {
          status: 500,
          statusText: 'Internal Server Error',
          body: { code: 'internal', error: 'reload failed' },
        };
      }
      return ok([]);
    });

    await userEvent.click(await screen.findByRole('button', { name: 'Configure Git forge' }));
    await screen.findByLabelText('base_url');
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));

    // `last_error`, verbatim, from a state the status code could not have told
    // us — and not painted as a kernel error, because `unavailable` is a
    // connector's normal terminal state.
    const status = await screen.findByText(new RegExp(reason.replaceAll('.', '\\.')));
    expect(status.textContent).toMatch(/did not come up/);
    expect(calls.filter((call) => call.path === '/api/plugins/git-forge').length).toBeGreaterThan(1);
  });

  it('offers no configuration entry point for a plugin that declares none', async () => {
    const calls = renderPlugins((request) => {
      if (request.path === '/api/plugins') return ok([{ ...ROW, has_config: false }]);
      return baseline(request) ?? ok([]);
    });

    await screen.findByText('Git forge');
    expect(screen.queryByRole('button', { name: 'Configure Git forge' })).toBeNull();
    // And nothing fetched its detail speculatively to find out.
    expect(calls.some((call) => call.path === '/api/plugins/git-forge')).toBe(false);
  });
});
