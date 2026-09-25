// The Planner provider choice on the real new-track route, in a real engine: what a switch
// resets, what the create carries, and that the row still fits a phone (#1791 PR5).
import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 720); });

const AREA = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const LIVE_CATALOG = {
  models: [{ id: 'fast', model: 'gpt-5', display_name: 'GPT-5', description: '', is_default: true,
    default_reasoning_effort: 'low', supported_reasoning_efforts: [
      { reasoning_effort: 'low', description: 'Answers sooner' },
      { reasoning_effort: 'high', description: 'Thinks longer' },
    ] }],
  default: { model: null, reasoning_effort: null }, default_source: 'unknown', source: 'live', fetched_at_ms: 1,
};
/* What `routes/models.rs` answers for `?provider=claude`. */
const CLAUDE_CATALOG = {
  models: [], default: { model: null, reasoning_effort: null }, default_source: 'unknown',
  source: 'unavailable', fetched_at_ms: null,
};
const REFUSAL = 'bad request: track create: `planner_provider` `claude` is unavailable: '
  + 'calm-server was started without --claude-planner-config';

function mount() {
  const creates: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    if (request.method === 'POST' && request.path === '/api/tracks') {
      creates.push(request);
      return Promise.resolve({ status: 400, statusText: 'Bad Request', body: { error: REFUSAL, code: 'bad_request' } });
    }
    if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
    if (request.path === '/api/models?provider=codex') return Promise.resolve(ok(LIVE_CATALOG));
    if (request.path === '/api/models?provider=claude') return Promise.resolve(ok(CLAUDE_CATALOG));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(), unauthorized, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/area/c1/new'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return { creates };
}

it.each([1280, 390])('switching to Claude clears the retained model and effort, and the create carries claude (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const { creates } = mount();
  const model = page.getByRole('button', { name: /^Model: / });
  await model.click();
  await page.getByRole('menuitem', { name: 'GPT-5' }).click();
  if (width < 600) {
    /* Phone: effort lives inside the model menu. */
    (await page.getByRole('button', { name: 'Model: GPT-5' }).findElement()).focus();
    await userEvent.keyboard('{ArrowDown}');
  } else {
    await page.getByRole('button', { name: 'Reasoning effort: low (the default)' }).click();
  }
  await page.getByRole('menuitem', { name: /high/ }).click();

  await page.getByRole('button', { name: 'Planner: Codex' }).click();
  await page.getByRole('menuitem', { name: /^Claude/ }).click();
  await expect.element(page.getByRole('button', { name: 'Planner: Claude' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Model: Default' })).toBeDisabled();
  await expect.element(page.getByRole('button', { name: /^Reasoning effort:/ })).not.toBeInTheDocument();

  /* Provider, model and Create share one row, with no horizontal scroll. */
  const provider = (await page.getByRole('button', { name: 'Planner: Claude' }).findElement()).getBoundingClientRect();
  const send = (await page.getByRole('button', { name: 'Create track' }).findElement()).getBoundingClientRect();
  expect(Math.abs(provider.top + provider.height / 2 - send.top - send.height / 2)).toBeLessThan(2);
  expect(provider.right).toBeLessThanOrEqual(send.left);
  expect(send.right).toBeLessThanOrEqual(width);
  expect(document.documentElement.scrollWidth).toBe(width);

  await page.getByRole('textbox', { name: 'What this track should do' }).fill('Plan with Claude');
  await page.getByRole('button', { name: 'Create track' }).click();
  await expect.element(page.getByRole('alert')).toHaveTextContent('--claude-planner-config');
  expect(creates).toHaveLength(1);
  expect(creates[0]?.body).toMatchObject({ planner_provider: 'claude', first_message: 'Plan with Claude' });
  expect(creates[0]?.body).not.toHaveProperty('model');
  expect(creates[0]?.body).not.toHaveProperty('reasoning_effort');

  /* Back on Codex the picker is live again, at the installation default rather than the old choice. */
  (await page.getByRole('button', { name: 'Planner: Claude' }).findElement()).focus();
  await userEvent.keyboard('{ArrowDown}');
  await page.getByRole('menuitem', { name: /^Codex/ }).click();
  await expect.element(page.getByRole('button', { name: 'Model: Default' })).toBeEnabled();
});
