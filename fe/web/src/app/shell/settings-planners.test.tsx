// @vitest-environment jsdom
// Settings › Planners through the real router and query layer (#1817): the pane reads
// `GET /api/agent-providers`, and Recheck asks the server to run every check again.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { settingsSectionForPath } from './settings-overlay.tsx';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const LOGGED_OUT = 'not logged in — run `claude /login` with CLAUDE_CONFIG_DIR=/srv/claude';

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

function answer(claude: Readonly<{ status: string; reason: string | null }>) {
  return [
    { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1_760_000_000_000 },
    { provider: 'claude', ...claude, checked_at_ms: 1_760_000_000_000 },
  ];
}

function renderPlanners() {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request): Promise<ApiTransportResponse> {
      sent.push(request);
      const body = request.path === '/api/agent-providers'
        ? answer({ status: 'unavailable', reason: LOGGED_OUT })
        : request.path === '/api/agent-providers?refresh=true'
          ? answer({ status: 'ready', reason: null })
          : request.path === '/api/settings' ? { settings: {} } : [];
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined,
  });
  router.update({ history: createMemoryHistory({ initialEntries: ['/settings/planners'] }) });
  render(
    <QueryClientProvider client={client}>
      <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return { sent };
}

function row(title: string): HTMLElement {
  const heading = screen.getByText(title, { selector: '*' });
  const item = heading.closest('li');
  if (item === null) throw new Error(`no row for ${title}`);
  return item;
}

it('maps the Planners path to its pane', () => {
  expect(settingsSectionForPath('/settings/planners')).toBe('planners');
});

it('lists each provider with its status and reason, and Recheck replaces the answer', async () => {
  const { sent } = renderPlanners();
  await screen.findByText(LOGGED_OUT);
  expect(within(row('Claude')).getByText('Unavailable')).toBeTruthy();
  expect(within(row('Codex')).getByText('Ready')).toBeTruthy();
  expect(sent.filter((request) => request.path === '/api/agent-providers?refresh=true')).toHaveLength(0);

  await userEvent.click(screen.getByRole('button', { name: 'Recheck planners' }));
  await waitFor(() => expect(screen.queryByText(LOGGED_OUT)).toBeNull());
  expect(within(row('Claude')).getByText('Ready')).toBeTruthy();
  expect(sent.filter((request) => request.path === '/api/agent-providers?refresh=true')).toHaveLength(1);
});
