import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import '../../styles/entry.css';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

it.each([
  ['light', 1080], ['light', 390], ['dark', 1080], ['dark', 390],
] as const)('recovers the recipe page with a stable pending action in %s at %ipx', async (theme, width) => {
  await page.viewport(width, 844);
  let reads = 0;
  let finishRetry!: (response: ApiTransportResponse) => void;
  const retry = new Promise<ApiTransportResponse>((resolve) => { finishRetry = resolve; });
  const transport: ApiTransportPort = { send(request) {
    if (request.path === '/api/track-recipes') {
      reads += 1;
      return reads === 1
        ? Promise.resolve({ status: 500, statusText: 'Internal Server Error', body: { error: 'Storage is offline.' } })
        : retry;
    }
    return Promise.resolve({ status: 200, statusText: 'OK', body: [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => {} });
  window.history.pushState({}, '', `${APP_BASEPATH}/recipes`);
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => theme, setItem: () => {} }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  await expect.element(page.getByText('Could not load your recipes.')).toBeVisible();
  const action = screen.getByRole('button', { name: 'Retry' });
  const height = action.getBoundingClientRect().height;
  await userEvent.click(action);
  await expect.poll(() => action.getAttribute('aria-busy')).toBe('true');
  expect(document.activeElement).toBe(action);
  expect(action.getBoundingClientRect().height).toBe(height);
  // Exercise the handler guard even when the driver sees aria-disabled.
  await userEvent.click(action, { force: true });
  expect(reads).toBe(2);
  await page.screenshot({ path: `__screenshots__/issue2046-retry-${theme}-${width}.png` });
  await act(async () => { finishRetry({ status: 200, statusText: 'OK', body: [{
    id: 'recipe-one', title: 'Recovered recipe', body: 'Stored content.',
    revision: 1, created_at: 1, updated_at: 1,
  }] }); await retry; });
  await expect.element(page.getByRole('button', { name: 'Recovered recipe' })).toBeVisible();
  expect(screen.queryByText('Could not load your recipes.')).toBeNull();
  client.clear();
});
