import { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { AppProviders, WEB_COMPAT_VERSION, type ProviderRuntime } from './public.tsx';

afterEach(cleanup);

it.each([390, 1280])('keeps preflight recovery compact and navigation usable at %i px', async (width) => {
  await page.viewport(width, 900);
  const transport: ApiTransportPort = { send: (request) => Promise.resolve({
    status: 200, statusText: 'OK', body: request.path === '/api/today/launchpad' ? null
      : request.path === '/api/areas' ? [{
        id: 'a1', name: 'Product', color: '#123456', sort: 1, kind: 'user',
        default_template_id: null, default_cwd: null, created_at: 1, updated_at: 1,
      }] : [],
  }) };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, retryDelay: 0 } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  const runtime: ProviderRuntime = {
    fetchVersion: vi.fn().mockRejectedValueOnce(new Error('Version service unavailable'))
      .mockRejectedValueOnce(new Error('Version service unavailable')).mockResolvedValue({
        webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
        syncEventVersion: 3, dbInstanceId: 'browser-recovery',
      }),
    reload: vi.fn(), deleteDatabase: vi.fn(), idbDatabaseName: 'browser-recovery',
    storage: { getItem: (key) => key === DB_INSTANCE_ID_KEY ? 'browser-recovery' : null, setItem: vi.fn(), removeItem: vi.fn() },
  };
  render(<AppProviders client={client} runtime={runtime} cursorStore={{ clear: vi.fn() }}>
    <RouterProvider router={router} />
  </AppProviders>);
  const retry = page.getByRole('button', { name: 'Retry live updates' });
  await expect.element(retry).toBeVisible();
  const status = retry.element().parentElement!;
  const rect = status.getBoundingClientRect();
  expect(rect.width).toBeLessThan(width - 16);
  expect(rect.height).toBeLessThan(70);
  await expect.element(page.getByRole('navigation', { name: width < 960 ? 'Primary' : 'Workspace' })).toBeVisible();
  await page.screenshot({ path: `../../../../test-results/preflight-recovery-${width}.png` });
  await retry.click();
  await expect.element(retry).not.toBeInTheDocument();
  await expect.element(page.getByRole('navigation', { name: width < 960 ? 'Primary' : 'Workspace' })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
});
