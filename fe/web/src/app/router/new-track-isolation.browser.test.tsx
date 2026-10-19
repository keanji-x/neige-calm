import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import '../../styles/entry.css';

afterEach(cleanup);

function mount() {
  const creates: ApiRequest[] = [];
  const transport: ApiTransportPort = { send(request) {
    if (request.method === 'POST' && request.path === '/api/tracks') {
      creates.push(request);
      return Promise.resolve({ status: 400, statusText: 'Bad Request', body: {
        code: 'bad_request', error: 'Folder overlaps Neige-owned location /repo/.claude/worktrees/track-other',
      } });
    }
    const body = request.path === '/api/areas' ? [{ id: 'a', name: 'Work', color: '#5B8DEF',
      sort: 0, kind: 'user', default_cwd: '/repo', created_at: 1, updated_at: 1 }]
      : request.path === '/api/settings' ? {} : [];
    return Promise.resolve({ status: 200, statusText: 'OK', body });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/area/a/new'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return creates;
}

it.each([1280, 390])('composer isolation option, badge, explanation and admission refusal (%ipx)', async (width) => {
  await page.viewport(width, 900);
  const creates = mount();
  const trigger = page.getByRole('button', { name: 'Workspace isolation: Worktree' });
  await expect.element(trigger).toBeVisible();
  expect(trigger.element().closest('[data-nc-composer-preferences]')).toBeTruthy();
  await trigger.click();
  await expect.element(page.getByRole('menuitem', { name: /Worktree.*Selected/ })).toBeVisible();
  await page.getByRole('menuitem', { name: /Worktree/ }).click();
  await page.getByRole('combobox', { name: 'What this track should do' }).fill('Plan');
  await page.getByRole('button', { name: 'Create track' }).click();
  await expect.poll(() => creates.length).toBe(1);
  expect(creates[0]?.body).not.toHaveProperty('isolation');

  await trigger.click();
  await page.getByRole('menuitem', { name: 'None' }).click();
  const badge = page.getByRole('button', { name: 'Not isolated · no guarantees' });
  await expect.element(badge).toBeVisible();
  await badge.click();
  await expect.element(page.getByText(/Planner commits land on its current branch/)).toBeVisible();
  await expect.element(page.getByText(/task workers are unavailable/i)).toBeVisible();
  await expect.element(page.getByText(/#2862/)).toBeVisible();
  await badge.click();
  await page.getByRole('button', { name: 'Create track' }).click();
  await expect.poll(() => creates.length).toBe(2);
  expect(creates[1]?.body).toMatchObject({ cwd: '/repo', isolation: 'none', first_message: 'Plan' });
  await expect.element(page.getByText('Folder overlaps Neige-owned location /repo/.claude/worktrees/track-other')).toBeVisible();
  expect(document.documentElement.scrollWidth).toBe(width);
  if (width === 390) await page.getByRole('button', { name: 'Track options', exact: true }).click();
  await page.getByRole('button', { name: 'Use a Neige workspace instead' }).click();
  if (width === 390) await page.getByRole('button', { name: 'Track options', exact: true }).click();
  const privateTrigger = page.getByRole('button', { name: 'Workspace isolation: None' });
  await expect.element(privateTrigger).toBeVisible();
  await expect.element(badge).not.toBeInTheDocument();
  await privateTrigger.click();
  const disabled = page.getByRole('menuitem', { name: /Worktree/ });
  await expect.element(disabled).toHaveAttribute('aria-disabled', 'true');
  await expect.element(page.getByText('Private Neige folder: no worktree. Choose a Git folder to use one.')).toBeVisible();
  await expect.element(page.getByRole('menuitem', { name: /None.*Selected/ })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBe(width);
});
