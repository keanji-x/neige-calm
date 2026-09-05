// @vitest-environment jsdom
//
// #1242 — one claim: after an enable or a disable **that succeeded**, the row
// says where that change stops, and after one that failed it says nothing of
// the kind.
//
// ## Why this drives the real hook and the real pane
//
// The line is a collaboration, not a component: `usePluginMutations` decides
// *when* a plugin has a settled, successful write, and `PluginsPane` decides
// what that looks like on the row. A test that handed the pane a hand-built set
// would prove only that a prop is rendered, and would stay green if the hook
// set the flag on failure, on dispatch, or never — which is the whole of what
// can go wrong here. So the transport is the fake and everything above it is
// production wiring: the real query options, the real mutation hook, the real
// `settings-overlay` prop hand-off is the only line restated (`app/shell` is
// the composition, and importing a route shell would drag the router in).
//
// ## The live region is always mounted, so the assertion is on its text
//
// Every row carries the region from first paint — a region inserted together
// with its text is commonly not announced — so "no line here" is an empty
// region, not an absent one, and the negative case says exactly that. While a
// write is in flight the row also holds astryx's own visually-hidden "Loading"
// region from the busy Switch; the two never speak at once, because this one is
// cleared in `onMutate` and can only be refilled after the write settles, by
// which point the Switch's region has unmounted. Every assertion below runs
// after `settle()`.
//
// The region is located by `data-nc-effect-boundary`, not by role: #1480 gave
// every row a Remove button, and astryx renders a visually-hidden
// `role="status"` region inside every button, so "the status region in this
// row" stopped being a unique description. The *role* is still what has to
// hold, and `boundaryIn` asserts it on the element it found.
//
// ## Why the assertion is a role plus a fragment
//
// Asserted as `role="status"` on the plugin's own row, with the fragment
// `already in progress` — the clause that carries the claim. The full sentence
// is deliberately not pinned: this is copy, it will be edited, and a suite that
// goes red on a comma teaches people to stop reading it. The role is what
// actually has to hold — a live region, not an alert, announced after a write
// that worked — and the fragment is what stops "a status element exists" from
// passing on some unrelated line. `takes effect` would have been the wrong
// anchor: it survives a rewrite into a sentence about tools, which is the one
// edit that must not pass.
import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { PluginsPane } from '../../features/settings/plugins.tsx';
import { pluginsQueryOptions, usePluginMutations } from './queries.ts';

beforeEach(() => {
  // Astryx's Switch spinner asks for `matchMedia`; jsdom has none. Stubbed
  // here, never globally — `app/theme` branches on its absence.
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn(),
  })));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

/**
 * A kernel that holds the enabled bit, and refuses to write it for whichever
 * plugins the case names. The refusal is a transport failure rather than a 500
 * body because the two arrive at `onError` identically and this file is not
 * about which one the kernel sent.
 */
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

/**
 * The row a plugin's id sits on. Found through the id — the one per-row string
 * this pane guarantees is unique and is not a presentation class, which the
 * DOM-query rule forbids reaching for.
 */
/**
 * This row's effect-boundary region, with its role checked on the way out.
 *
 * The role assertion is not decoration: what the contract requires is a polite
 * live region, and locating the element by a data attribute would otherwise let
 * a change from `status` to `alert` — or to no role at all — pass unnoticed.
 */
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

/* A macrotask, not a microtask: the read, the write and the invalidation's
   refetch each land on their own turn, and a `Promise.resolve()` flush stops
   after the first one — leaving the pane still painting "Loading plugins…". */
async function settle() {
  await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 0); }); });
}

/* Both directions in one render, so neither can pass on a branch that only
   handles the other: `todo` starts off and is switched on, `git-forge` starts
   on and is switched off. */
it('states the effect boundary on a row whose enable succeeded, and on one whose disable succeeded', async () => {
  const { enabled } = await mount();

  await userEvent.click(await screen.findByRole('switch', { name: 'Enable Todo' }));
  await settle();
  expect(enabled.get('todo')).toBe(true);
  expect(boundaryIn(row('todo')).textContent).toContain('already in progress');
  // And only on the row that was written: the flag is per plugin, so the other
  // row's region is mounted and empty.
  expect(boundaryIn(row('git-forge')).textContent).toBe('');

  await userEvent.click(await screen.findByRole('switch', { name: 'Enable Git forge' }));
  await settle();
  expect(enabled.get('git-forge')).toBe(false);
  // A disable is the same boundary — an in-flight conversation still holds the
  // tool list it started with — so the line is not conditioned on `enabled`.
  expect(boundaryIn(row('git-forge')).textContent).toContain('already in progress');
});

it('says nothing about the boundary when the write failed', async () => {
  await mount(new Set(['todo']));

  await userEvent.click(await screen.findByRole('switch', { name: 'Enable Todo' }));
  await settle();

  /* The failure is reported, and reported as a failure. The *text* is the
     transport's, not this fixture's — `runOperation` reduces a rejected send to
     its own wording — so what is pinned is that the row carries an alert, which
     is the half this file's claim rests on. */
  expect(within(row('todo')).getByRole('alert').textContent).not.toBe('');
  /* Nothing changed, so there is no reach to describe. A boundary line here
     would be a claim about a change that never happened. The live region is
     still mounted — it is on every row from first paint, so that a message can
     be announced when there is one — and what has to hold is that it is
     empty. */
  expect(boundaryIn(row('todo')).textContent).toBe('');
  expect(screen.queryAllByText(/already in progress/).length).toBe(0);
});
