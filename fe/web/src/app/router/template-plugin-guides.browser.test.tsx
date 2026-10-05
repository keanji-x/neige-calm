import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 720); });

function mount(issueGuides?: Promise<ApiTransportResponse>) {
  const requests: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    if (request.path === '/api/areas') return Promise.resolve(ok([{ id: 'area', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }]));
    if (request.path === '/api/track-templates') return Promise.resolve(ok([
      { id: 'dev', title: 'Development', tasks: [] },
      { id: 'small-change', title: 'Small change', tasks: [] },
    ]));
    if (request.path.endsWith('/dev/plugin-guides') && issueGuides !== undefined) return issueGuides;
    if (request.path.endsWith('/plugin-guides')) return Promise.resolve(ok(request.path.includes('/dev/')
      ? [{ id: 'gitforge', name: 'development' }] : []));
    if (request.path.startsWith('/api/track-templates/')) return Promise.resolve(ok({
      id: request.path.split('/').at(-1), title: 'Template', description: null, instructions: null, body: '# Method',
    }));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/area/area/new'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return requests;
}

it('shows the locked template guide in the composer and removes it when the template changes', async () => {
  await page.viewport(1440, 900);
  const requests = mount();
  const field = page.getByRole('combobox', { name: 'What this track should do' });
  await field.fill('Keep this sentence');
  await page.getByRole('button', { name: /^Template:/ }).click();
  await page.getByRole('menuitem', { name: /^Development/ }).click();
  await expect.element(page.getByLabelText('development, included by template')).toBeVisible();
  expect(document.querySelector('[aria-label="development, included by template"] svg')).not.toBeNull();
  await expect.poll(() => {
    const guide = page.getByLabelText('development, included by template').element().getBoundingClientRect();
    const input = field.element().getBoundingClientRect();
    return guide.right <= input.left && guide.bottom > input.top && input.bottom > guide.top;
  }).toBe(true);
  await expect.element(field).toHaveTextContent('Keep this sentence');
  for (const width of [390, 320]) {
    await page.viewport(width, 844);
    await expect.element(page.getByLabelText('development, included by template')).toBeVisible();
    expect(document.documentElement.scrollWidth).toBe(window.innerWidth);
  }
  await page.viewport(1440, 900);
  await page.getByRole('button', { name: /^Template:/ }).click();
  await page.getByRole('menuitem', { name: /^No template/ }).click();
  await expect.element(page.getByLabelText('development, included by template')).not.toBeInTheDocument();
  await expect.element(field).toHaveTextContent('Keep this sentence');
  expect(requests.every(request => request.method === 'GET')).toBe(true);
});

it('does not revive a default guide when its read finishes after switching templates', async () => {
  await page.viewport(1440, 900);
  let resolve!: (response: ApiTransportResponse) => void;
  const late = new Promise<ApiTransportResponse>(done => { resolve = done; });
  const requests = mount(late);
  await page.getByRole('button', { name: /^Template:/ }).click();
  await page.getByRole('menuitem', { name: /^Development/ }).click();
  await expect.poll(() => requests.some(request => request.path.endsWith('/dev/plugin-guides'))).toBe(true);
  await page.getByRole('button', { name: /^Template:/ }).click();
  await page.getByRole('menuitem', { name: /^No template/ }).click();
  resolve({ status: 200, statusText: 'OK', body: [{ id: 'gitforge', name: 'development' }] });
  await expect.element(page.getByLabelText('development, included by template')).not.toBeInTheDocument();
  await expect.element(page.getByRole('button', { name: /^Template: No template/ })).toBeVisible();
});
