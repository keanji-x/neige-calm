import { prepareReportDocument } from '../../features/report/document/preparation.ts';
// The new-track route's Role picker in a real engine: Member by default, beside the model picker at every width, and
// `admin: true` reaches `POST /api/tracks` only for Admin.
import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(async () => { cleanup(); await page.viewport(1280, 720); });

const AREA = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };

function mount() {
  const creates: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    if (request.method === 'POST' && request.path === '/api/tracks') {
      creates.push(request);
      /* Held: the page stays put, so the request is all there is to read. */
      return new Promise<ApiTransportResponse>(() => undefined);
    }
    if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/area/c1/new'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return { creates };
}

const roleTrigger = (role: string) => page.getByRole('button', { name: `Role: ${role}` });

async function create(text: string): Promise<void> {
  await page.getByRole('combobox', { name: 'What this track should do' }).fill(text);
  await page.getByRole('button', { name: 'Create track' }).click();
}

it.each([[1280, 844], [390, 844], [320, 700]])('Role: Admin beside the model picker posts `admin: true` (%ipx)', async (width, height) => {
  await page.viewport(width, height);
  const { creates } = mount();
  await expect.element(roleTrigger('Member')).toBeVisible();
  /* Beside the model picker, on the send row, and inside the page. */
  const model = page.getByRole('button', { name: /^Model: / }).element().getBoundingClientRect();
  const role = roleTrigger('Member').element().getBoundingClientRect();
  const send = page.getByRole('button', { name: 'Create track' }).element().getBoundingClientRect();
  expect(model.right).toBeLessThanOrEqual(role.left);
  expect(role.right).toBeLessThanOrEqual(send.left);
  expect(Math.abs(role.top + role.height / 2 - send.top - send.height / 2)).toBeLessThan(2);
  expect(send.right).toBeLessThanOrEqual(width);
  expect(document.documentElement.scrollWidth).toBe(width);
  await roleTrigger('Member').click();
  const admin = page.getByRole('menuitem', { name: /^Admin/ });
  await expect.element(admin).toHaveTextContent('AdminMay add Tracks and close the ones it added.');
  await expect.element(page.getByRole('menuitem', { name: /^Member/ })).toHaveTextContent('MemberAn ordinary Planner.Selected');
  for (const item of page.getByRole('menuitem').elements()) expect(item.getBoundingClientRect().right).toBeLessThanOrEqual(width);
  await admin.click();
  await expect.element(roleTrigger('Admin')).toBeVisible();
  expect(page.getByRole('checkbox').elements()).toEqual([]);
  await create('Run the area');
  await expect.poll(() => creates.length).toBe(1);
  expect(creates[0]?.body).toMatchObject({ area_id: 'c1', admin: true, first_message: 'Run the area' });
  /* Locked with the model picker while the create is in flight. */
  await expect.element(roleTrigger('Admin')).toBeDisabled();
  expect(document.documentElement.scrollWidth).toBe(width);
});

it('the default Member role sends no `admin` key', async () => {
  const { creates } = mount();
  await expect.element(roleTrigger('Member')).toBeVisible();
  await create('Just a track');
  await expect.poll(() => creates.length).toBe(1);
  expect(creates[0]?.body).toMatchObject({ area_id: 'c1', first_message: 'Just a track' });
  expect(creates[0]?.body).not.toHaveProperty('admin');
});
