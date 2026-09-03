// @vitest-environment jsdom
//
// The claim this file exists for: **the Settings panel is one element for the
// whole visit**, not one per section.
//
// The first cut returned a `<Dialog>` from each settings route component, which
// is a different element at a different place in the tree per route. Moving
// from General to Plugins therefore unmounted one panel and mounted another,
// the `dialog-enter` animation replayed, and the reader saw the dialog flash on
// every click of its own navigation. Nothing failed: every assertion about
// *content* still passed, because the content was correct — it was the element
// identity that was wrong.
//
// So this asserts identity directly, by marking the panel node and requiring
// the same node back after the navigation. A component test cannot do it; only
// the real router can produce the navigation that used to destroy it.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { settingsSectionForPath } from './settings-overlay.tsx';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

beforeEach(() => {
  // Astryx's Spinner calls `matchMedia` unguarded; `app/theme` deliberately
  // branches on it being absent, so this is stubbed per file, never globally.
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

function bodyFor(path: string): unknown {
  if (path === '/api/areas') return [];
  if (path === '/api/settings') return { settings: {} };
  if (path === '/api/plugins') return [];
  if (path === '/api/track-templates') return [];
  return [];
}

function renderApp(initialEntry: string) {
  const transport: ApiTransportPort = {
    send(request): Promise<ApiTransportResponse> {
      return Promise.resolve({ status: 200, statusText: 'OK', body: bodyFor(request.path) });
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined,
  });
  router.update({ history: createMemoryHistory({ initialEntries: [initialEntry] }) });
  render(
    <QueryClientProvider client={client}>
      <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return router;
}

describe('settingsSectionForPath', () => {
  it('maps every settings path to a pane and nothing else to one', () => {
    expect(settingsSectionForPath('/settings')).toBe('network');
    expect(settingsSectionForPath('/settings/appearance')).toBe('appearance');
    expect(settingsSectionForPath('/settings/plugins')).toBe('plugins');
    expect(settingsSectionForPath('/settings/about')).toBe('about');
    // #1300 S1 — the two template routes are gone with the editor. This is the
    // negative half of that removal: a stale bookmark to either one must leave
    // the dialog shut, not open it on some fallback pane. Deleting a route
    // without asserting it is gone is how a removal quietly comes back.
    expect(settingsSectionForPath('/settings/templates')).toBeNull();
    expect(settingsSectionForPath('/settings/templates/issue-development')).toBeNull();
    // The dialog stays shut everywhere else — including on a path that merely
    // starts with the same letters.
    expect(settingsSectionForPath('/')).toBeNull();
    expect(settingsSectionForPath('/track/w1')).toBeNull();
    expect(settingsSectionForPath('/settingsish')).toBeNull();
  });
});

describe('Settings overlay', () => {
  it('keeps one panel element across its own section navigation', async () => {
    renderApp('/settings');
    const dialog = await screen.findByRole('dialog', { name: 'Settings' });
    dialog.setAttribute('data-nc-test-marked', '');

    await userEvent.click(screen.getByRole('button', { name: 'Plugins' }));
    await screen.findByRole('heading', { name: 'Plugins' });
    // Same node, not merely a node: a remounted panel replays the entrance
    // animation, which is the flash this move removed.
    expect(screen.getByRole('dialog', { name: 'Settings' })).toBe(dialog);

    await userEvent.click(screen.getByRole('button', { name: 'Appearance' }));
    await screen.findByRole('heading', { name: 'Appearance' });
    expect(screen.getByRole('dialog', { name: 'Settings' })).toBe(dialog);
    expect(document.querySelectorAll('[role="dialog"][data-nc-test-marked]')).toHaveLength(1);
  });

  it('opens on a deep link and closes to Today', async () => {
    const router = renderApp('/settings/plugins');
    await screen.findByRole('dialog', { name: 'Settings' });
    expect(screen.getByRole('button', { name: 'Plugins' }).getAttribute('aria-current')).toBe('page');

    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Settings' })).toBeNull());
    expect(router.state.location.pathname).toBe('/');
  });

  it('is shut on a route that is not Settings', async () => {
    renderApp('/');
    await screen.findByRole('navigation', { name: 'Workspace' });
    expect(screen.queryByRole('dialog', { name: 'Settings' })).toBeNull();
  });
});
