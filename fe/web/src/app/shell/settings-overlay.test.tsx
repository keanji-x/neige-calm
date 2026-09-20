// @vitest-environment jsdom
// The Settings panel is one element for the whole visit, not one per section; only
// the real router can produce the navigation that would destroy it.
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
    expect(settingsSectionForPath('/settings')).toBe('general');
    expect(settingsSectionForPath('/settings/general')).toBe('general');
    expect(settingsSectionForPath('/settings/network')).toBe('network');
    expect(settingsSectionForPath('/settings/appearance')).toBe('appearance');
    expect(settingsSectionForPath('/settings/plugins')).toBe('plugins');
    expect(settingsSectionForPath('/settings/about')).toBe('about');
    // A stale bookmark to a removed route must leave the dialog shut, not open it on a fallback pane.
    expect(settingsSectionForPath('/settings/templates')).toBeNull();
    expect(settingsSectionForPath('/settings/templates/issue-development')).toBeNull();
    // Shut everywhere else, including a path that merely starts with the same letters.
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
    // Same node, not merely a node: a remounted panel replays the entrance animation.
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
