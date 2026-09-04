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
//      status code** (§2.4), on *both* branches. One case has the reload answer
//      500 with a body that says nothing useful; the other has it answer 200
//      saying `running`. Either way the plugin is `unavailable` with a
//      `last_error` when it is read back, and that is the sentence asserted —
//      neither status code can produce it, so both assertions are satisfiable
//      only by the extra read.
//   3. **a confirmation outlives the refetch the write itself triggers.** Both
//      writes invalidate this plugin's detail before resolving, so the pane
//      re-seeds from a changed `user_config` in the same breath as it renders
//      the sentence. See `pluginRow` for why a frozen fixture cannot see this.
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

/**
 * ── The fixture has to be a row, not a constant ───────────────────────────
 *
 * (#1284 S4 review P1-B.) These tests used to answer every `GET
 * /api/plugins/{id}` with the *same* `detailBody()`, so a PATCH that stored
 * something never showed up in any later read. That is not a simplification, it
 * is the one difference that made a production defect invisible: both writes
 * invalidate this plugin's detail before they resolve, the successful write's
 * own refetch therefore lands a changed `user_config`, and the pane re-seeds
 * off it — which used to clear the phase and erase the confirmation the write
 * had just produced. Only the success path could reach it (a refusal changes
 * nothing stored), which is exactly the path a frozen fixture cannot tell apart
 * from a working one.
 *
 * So the fixture keeps a row and the PATCH writes into it, with `null`
 * deleting a key the way the kernel does. Nothing else here changes; every
 * assertion below now runs against a detail that moves.
 */
function pluginRow(initial: Record<string, unknown> = { token: 'stored' }) {
  let userConfig: Record<string, unknown> = { ...initial };
  return {
    detail: (overrides: Record<string, unknown> = {}) =>
      detailBody({ user_config: { ...userConfig }, ...overrides }),
    /** Applies a patch body the way `patch_plugin_config` does. */
    patch(body: unknown, reset: boolean) {
      const next: Record<string, unknown> = reset ? {} : { ...userConfig };
      for (const [key, value] of Object.entries((body ?? {}) as Record<string, unknown>)) {
        if (value === null) delete next[key];
        else next[key] = value;
      }
      userConfig = next;
    },
    get stored() { return userConfig; },
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
    const row = pluginRow();
    const calls = renderPlugins((request) => {
      const shared = baseline(request);
      if (shared !== null) return shared;
      if (request.path === '/api/plugins/git-forge') return ok(row.detail());
      if (request.path === '/api/plugins/git-forge/config') {
        row.patch(request.body, false);
        return ok(row.detail());
      }
      if (request.path === '/api/plugins/git-forge/reload') return ok(row.detail({ state: 'running' }));
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
    // The write landed in the fixture's row, which is what makes the sentence
    // above a claim about a pane that survived its own refetch (P1-B).
    expect(row.stored).toEqual({ token: 'stored', base_url: 'https://forge.internal' });
  });

  /*
   * ── P1-B: the confirmation has to outlive the refetch it causes ───────────
   *
   * A plain Save, with nothing else going on. `save` invalidates this plugin's
   * detail before resolving, so by the time "Saved." is asked for, a refetch
   * carrying the *new* `user_config` has already landed and the pane has
   * re-seeded off it. This assertion is only reachable if re-seeding stopped
   * throwing the phase away; with the unconditional `setPhase(IDLE)` it went
   * back to `idle` and the sentence was never rendered.
   */
  it('keeps the confirmation after the refetch its own write triggered', async () => {
    const row = pluginRow();
    renderPlugins((request) => {
      const shared = baseline(request);
      if (shared !== null) return shared;
      if (request.path === '/api/plugins/git-forge') return ok(row.detail());
      if (request.path === '/api/plugins/git-forge/config') {
        row.patch(request.body, false);
        return ok(row.detail());
      }
      return ok([]);
    });

    await userEvent.click(await screen.findByRole('button', { name: 'Configure Git forge' }));
    await userEvent.type(await screen.findByLabelText('base_url'), 'https://forge.internal');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    expect(await screen.findByText('Saved. Apply & restart to run with it.')).toBeTruthy();
    // Still there after the invalidated queries have settled, not merely for
    // one paint.
    await screen.findByDisplayValue('https://forge.internal');
    expect(screen.getByText('Saved. Apply & restart to run with it.')).toBeTruthy();
  });

  /*
   * ── P1-A: a switch moved onto its default deletes the key ────────────────
   *
   * `verbose` defaults to `true` in the manifest and the row stores `false`.
   * Flipping it back is the operator saying "follow the manifest again", and
   * the only wire form of that is `null`: posting the literal `true` would
   * write today's default into the row, after which a manifest that changed it
   * could never reach this plugin again (§2.2.4). A switch cannot send "unset"
   * any other way — it has two positions and no clear.
   */
  it('deletes a stored boolean the operator moved back onto its default', async () => {
    const row = pluginRow({ token: 'stored', verbose: false });
    const calls = renderPlugins((request) => {
      const shared = baseline(request);
      if (shared !== null) return shared;
      if (request.path === '/api/plugins/git-forge') return ok(row.detail());
      if (request.path === '/api/plugins/git-forge/config') {
        row.patch(request.body, false);
        return ok(row.detail());
      }
      return ok([]);
    });

    await userEvent.click(await screen.findByRole('button', { name: 'Configure Git forge' }));
    await userEvent.click(await screen.findByRole('switch', { name: 'verbose' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    await screen.findByText('Saved. Apply & restart to run with it.');
    const patch = calls.find((call) => call.method === 'PATCH');
    expect(patch?.body).toEqual({ verbose: null });
    // The key is gone from the row, so the manifest default applies again —
    // and it is gone rather than rewritten to `true`.
    expect(row.stored).toEqual({ token: 'stored' });
  });

  /*
   * ── P1-C: a 2xx reload is not the verdict either ─────────────────────────
   *
   * The reload answers 200 with `running`, and the detail read immediately
   * after says `unavailable` with a reason — an ordinary sequence for a
   * connector whose bring-up completes after the handler returns. §2.4 asks for
   * the state read back *after the attempt*, on both branches; a host that
   * trusted the POST body would confirm "restarted with it" over the top of the
   * only diagnostic there is.
   */
  it('re-reads the plugin after a 2xx reload rather than trusting its body', async () => {
    const reason = 'mcp-http: connect to https://forge.internal failed: connection refused';
    const row = pluginRow();
    renderPlugins((request, calls) => {
      const shared = baseline(request);
      if (shared !== null) return shared;
      if (request.path === '/api/plugins/git-forge') {
        const restarted = calls.some((call) => call.path === '/api/plugins/git-forge/reload');
        return ok(restarted ? row.detail({ state: 'unavailable', last_error: reason }) : row.detail());
      }
      if (request.path === '/api/plugins/git-forge/config') {
        row.patch(request.body, false);
        return ok(row.detail());
      }
      // 200, and it says the plugin is up.
      if (request.path === '/api/plugins/git-forge/reload') return ok(row.detail({ state: 'running' }));
      return ok([]);
    });

    await userEvent.click(await screen.findByRole('button', { name: 'Configure Git forge' }));
    await userEvent.type(await screen.findByLabelText('base_url'), 'https://forge.internal');
    await userEvent.click(screen.getByRole('button', { name: 'Apply & restart' }));

    const status = await screen.findByText(new RegExp(reason.replaceAll('.', '\\.')));
    expect(status.textContent).toMatch(/did not come up/);
    expect(screen.queryByText('Configuration saved and the plugin restarted with it.')).toBeNull();
  });

  it('reads the plugin back after a failed restart and reports what it found', async () => {
    const reason = 'mcp-http: connect to https://forge.internal failed: connection refused';
    const row = pluginRow();
    const calls = renderPlugins((request) => {
      const shared = baseline(request);
      if (shared !== null) return shared;
      if (request.path === '/api/plugins/git-forge') {
        /* Before the reload the plugin is fine; after it, it is `unavailable`
           with the reason. The switch is keyed on whether the reload has been
           attempted, so the second read is the only way to this sentence. */
        const restarted = calls.some((call) => call.path === '/api/plugins/git-forge/reload');
        return ok(restarted
          ? row.detail({ state: 'unavailable', last_error: reason })
          : row.detail());
      }
      if (request.path === '/api/plugins/git-forge/config') {
        row.patch(request.body, false);
        return ok(row.detail());
      }
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
