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

afterEach(cleanup);
const DESCRIPTION = 'Review a change and produce a verified report with evidence, open questions and recommended next steps.';

it.each([1280, 390, 320])('repairs a template prerequisite and returns to the same draft at %ipx', async (width) => {
  await page.viewport(width, 844);
  let enabled = false;
  const requests: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    if (request.path === '/api/areas') return Promise.resolve(ok([{
      id: 'area', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1,
      default_template_id: null, default_cwd: '/work/example',
    }]));
    if (request.path === '/api/track-templates') return Promise.resolve(ok([{
      id: 'site/review', title: 'Review a change', description: DESCRIPTION, tasks: [],
      availability: enabled ? { status: 'ready' } : { status: 'requires_plugin', plugin_id: 'review-tools', plugin_name: 'Review tools' },
    }]));
    if (request.path.endsWith('/plugin-guides')) return Promise.resolve(ok([]));
    if (request.path.startsWith('/api/track-templates/')) return Promise.resolve(ok({
      id: 'site/review', title: 'Review a change', description: DESCRIPTION, instructions: null, body: '# Report',
    }));
    if (request.path === '/api/plugins/review-tools/enable') {
      enabled = true;
      return Promise.resolve(ok({ id: 'review-tools', enabled: true }));
    }
    if (request.path === '/api/plugins') return Promise.resolve(ok([{
      id: 'review-tools', version: '0.1.0', enabled, state: enabled ? 'running' : 'disabled', manifest_name: 'Review tools',
      has_config: false, can_uninstall: false, can_disable: true,
    }]));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/area/area/new'] }) });
  // The application-level providers own the draft; this is the actual routed form and Settings.
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  const field = page.getByRole('combobox', { name: 'What this track should do' });
  await expect.element(page.getByRole('region', { name: 'Choose a working method' }).getByText(DESCRIPTION)).toBeVisible();
  await page.screenshot({ path: `__screenshots__/onboarding-working-methods-${width}.png` });
  await field.fill('Review the retry behavior');
  await expect.element(page.getByRole('region', { name: 'Choose a working method' })).not.toBeInTheDocument();
  if (width < 600) await page.getByRole('button', { name: 'Track options' }).click();
  await page.getByRole('button', { name: /^Template:/ }).click();
  await expect.element(page.getByText(DESCRIPTION)).toBeVisible();
  await expect.element(page.getByText('Requires Review tools')).toBeVisible();
  expect(document.documentElement.scrollWidth).toBe(window.innerWidth);
  await page.screenshot({ path: `__screenshots__/onboarding-template-menu-${width}.png` });
  await page.getByRole('menuitem', { name: /^Review a change/ }).click();
  await expect.element(page.getByRole('button', { name: 'Create track' })).toBeDisabled();
  await page.screenshot({ path: `__screenshots__/onboarding-template-prerequisite-${width}.png` });
  await page.getByRole('button', { name: 'Open plugin settings' }).click();
  await expect.poll(() => router.state.location.pathname).toBe('/settings/plugins');
  await page.getByRole('switch', { name: 'Review tools' }).click();
  await expect.poll(() => enabled).toBe(true);
  if (width < 600) {
    await page.getByRole('button', { name: 'Back to Settings' }).click();
    await page.getByRole('button', { name: 'Back to workspace' }).click();
  } else await page.getByRole('dialog', { name: 'Settings', exact: true }).getByRole('button', { name: 'Close', exact: true }).click();
  await expect.poll(() => router.state.location.pathname).toBe('/area/area/new');
  await expect.element(field).toHaveTextContent('Review the retry behavior');
  await expect.element(page.getByRole('button', { name: 'Create track' })).toBeEnabled();
  if (width < 600) await page.getByRole('button', { name: 'Track options' }).click();
  await expect.element(page.getByRole('button', { name: 'Template: Review a change' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Folder: /work/example' })).toBeVisible();
  if (width < 600) await page.getByRole('button', { name: 'Track options' }).click();
  expect(requests.some(request => request.path === '/api/tracks' && request.method === 'POST')).toBe(false);
  expect(document.documentElement.scrollWidth).toBe(window.innerWidth);
  await page.screenshot({ path: `__screenshots__/onboarding-template-${width}.png` });
  client.clear();
});
