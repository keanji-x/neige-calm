// @vitest-environment jsdom
//
// Drives the real hook and the real pane over a fake transport: the boundary line is a collaboration
// between `usePluginMutations` and `PluginsPane`. The live region is on every row from first paint (a
// region inserted with its text is commonly not announced), located by `data-nc-effect-boundary` because
// astryx renders a `role="status"` inside every button; asserted by role plus the `already in progress` fragment.
import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { PluginsPane } from '../../features/settings/plugins.tsx';
import { pluginsQueryOptions, usePluginMutations } from './queries.ts';

beforeEach(() => {
  // Astryx's Switch spinner asks for `matchMedia`; stubbed here, never globally — `app/theme` branches on its absence.
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn(),
  })));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

/** A kernel that holds the enabled bit and refuses to write it for the named plugins. */
function fakeKernel(refuses: ReadonlySet<string>) {
  const enabled = new Map<string, boolean>([['todo', false], ['git-forge', true]]);
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      if (request.method === 'GET') {
        return Promise.resolve({
          status: 200,
          statusText: 'OK',
          body: [...enabled].map(([id, on]) => ({
            id,
            version: '0.1.0',
            enabled: on,
            state: on ? 'running' : 'disabled',
            manifest_name: id === 'todo' ? 'Todo' : 'Git forge',
            has_config: false,
          })),
        });
      }
      const match = /^\/api\/plugins\/([^/]+)\/(enable|disable)$/.exec(request.path);
      if (match === null) throw new Error(`unexpected write: ${request.path}`);
      const [, id, verb] = match as unknown as [string, string, 'enable' | 'disable'];
      if (refuses.has(id)) return Promise.reject(new Error('Kernel refused this write.'));
      enabled.set(id, verb === 'enable');
      return Promise.resolve({
        status: 200, statusText: 'OK', body: { id, enabled: verb === 'enable' },
      });
    },
  };
  return { transport, enabled };
}

function Host({ transport }: { transport: ApiTransportPort }) {
  const plugins = useQuery(pluginsQueryOptions(transport, unauthorized));
  const mutations = usePluginMutations(transport, unauthorized);
  return (
    <PluginsPane
      plugins={plugins.data}
      loadError={plugins.error instanceof Error ? plugins.error.message : null}
      onRetryLoad={() => { void plugins.refetch(); }}
      pendingIds={mutations.pendingIds}
      errors={mutations.errors}
      effectBoundaryIds={mutations.effectBoundaryIds}
      onSetEnabled={mutations.setEnabled}
      onAdd={() => {}}
      onUninstall={mutations.uninstall}
      onOpenConfig={() => {}}
    />
  );
}

/** This row's effect-boundary region; the role is asserted because locating by data attribute would let `status` → `alert` pass. */
function boundaryIn(scope: HTMLElement): HTMLElement {
  const line = scope.querySelector<HTMLElement>('[data-nc-effect-boundary]');
  if (line === null) throw new Error('row has no effect-boundary region');
  expect(line.getAttribute('role')).toBe('status');
  return line;
}

function row(id: string): HTMLElement {
  const meta = screen.getByText(id).parentElement;
  if (meta === null) throw new Error(`no row for ${id}`);
  return meta;
}

async function mount(refuses: ReadonlySet<string> = new Set()) {
  const { transport, enabled } = fakeKernel(refuses);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  render(<QueryClientProvider client={client}><Host transport={transport} /></QueryClientProvider>);
  await settle();
  return { enabled };
}

/* A macrotask, not a microtask: the read, the write and the refetch each land on their own turn. */
async function settle() {
  await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 0); }); });
}

/* Both directions in one render: `todo` starts off and is switched on, `git-forge` the reverse. */
it('states the effect boundary on a row whose enable succeeded, and on one whose disable succeeded', async () => {
  const { enabled } = await mount();

  await userEvent.click(await screen.findByRole('switch', { name: 'Enable Todo' }));
  await settle();
  expect(enabled.get('todo')).toBe(true);
  expect(boundaryIn(row('todo')).textContent).toContain('already in progress');
  // Only on the row that was written: the flag is per plugin.
  expect(boundaryIn(row('git-forge')).textContent).toBe('');

  await userEvent.click(await screen.findByRole('switch', { name: 'Enable Git forge' }));
  await settle();
  expect(enabled.get('git-forge')).toBe(false);
  // A disable is the same boundary — an in-flight conversation still holds the tool list it started with.
  expect(boundaryIn(row('git-forge')).textContent).toContain('already in progress');
});

it('says nothing about the boundary when the write failed', async () => {
  await mount(new Set(['todo']));

  await userEvent.click(await screen.findByRole('switch', { name: 'Enable Todo' }));
  await settle();

  /* The text is the transport's, not this fixture's, so what is pinned is that the row carries an alert. */
  expect(within(row('todo')).getByRole('alert').textContent).not.toBe('');
  /* The live region is mounted on every row from first paint; what has to hold is that it is empty. */
  expect(boundaryIn(row('todo')).textContent).toBe('');
  expect(screen.queryAllByText(/already in progress/).length).toBe(0);
});
