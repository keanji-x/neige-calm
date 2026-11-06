import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { CurrentUserScope } from '../auth/current-user.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';

afterEach(() => { cleanup(); document.body.replaceChildren(); });
function setup() {
  const sent: ApiRequest[] = [];
  const stored: Record<string, string> = {};
  const transport: ApiTransportPort = { send(request) {
    sent.push(request);
    if (request.method === 'PUT') {
      const patch = (request.body as { settings: Record<string, string | null> }).settings;
      for (const [key, value] of Object.entries(patch)) {
        if (value === null) delete stored[key]; else stored[key] = value;
      }
    }
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.path === '/api/settings' ? { settings: { ...stored } }
      : request.path.startsWith('/api/models') ? { source: 'live', default_source: 'unknown', fetched_at_ms: null,
        default: { model: null, reasoning_effort: null, supported_reasoning_efforts: null },
        models: [{ id: 'catalog', model: 'catalog-model', display_name: 'Catalog model', description: '', is_default: false,
          resolved_model: null, default_reasoning_effort: null, supported_reasoning_efforts: [{ reasoning_effort: 'catalog-effort', description: null }] }] }
        : [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport,
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), client,
    cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/settings/workers'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <CurrentUserScope value={{ userId: 'local-owner', displayName: 'Owner', role: 'member', sessionId: 'session' }}>
      <RouterProvider router={router} />
    </CurrentUserScope>
  </ThemeProvider></QueryClientProvider>);
  return { sent, router };
}

it.each([320, 390, 1180])('edits both fields by keyboard and explicitly saves or clears without overflow at %ipx', async width => {
  await page.viewport(width, 844);
  const { sent } = setup();
  const model = page.getByRole('textbox', { name: 'Codex model', exact: true });
  await model.fill('private-model');
  await userEvent.keyboard('{Enter}{Tab}');
  const effort = page.getByRole('textbox', { name: 'Codex effort', exact: true });
  await effort.fill('private-effort');
  await userEvent.keyboard('{Enter}{Tab}');
  expect(sent.filter(request => request.method === 'PUT')).toHaveLength(0);
  const save = page.getByRole('button', { name: 'Save Codex defaults' });
  await expect.element(save).toHaveFocus();
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => sent.filter(request => request.method === 'PUT').length).toBe(1);
  expect(sent.find(request => request.method === 'PUT')?.body).toEqual({ settings: {
    worker_model_defaults_codex: '{"model":"private-model","effort":"private-effort"}',
  } });
  await expect.element(model).toHaveValue('private-model');
  await expect.element(page.getByText(/existing Workers keep their original selection/)).toBeVisible();
  for (const field of [model, effort]) {
    const box = field.element().getBoundingClientRect();
    expect(box.left).toBeGreaterThanOrEqual(0);
    expect(box.right).toBeLessThanOrEqual(width);
  }
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.screenshot({ path: `../../../../test-results/settings-workers-${width}.png` });
  await page.getByRole('button', { name: 'Clear Codex defaults' }).click();
  await expect.element(model).toHaveValue('');
  await expect.element(effort).toHaveValue('');
  expect(sent.filter(request => request.method === 'PUT').at(-1)?.body).toEqual({ settings: { worker_model_defaults_codex: null } });
});

it('selects catalog suggestions by keyboard without sending until Save', async () => {
  await page.viewport(1180, 844);
  const { sent } = setup();
  const model = await page.getByRole('combobox', { name: 'Codex model catalog', exact: true }).findElement();
  (model as HTMLElement).focus();
  await userEvent.keyboard('{Enter}{ArrowDown}{Enter}');
  await expect.element(page.getByRole('textbox', { name: 'Codex model', exact: true })).toHaveValue('catalog-model');
  const effort = await page.getByRole('combobox', { name: 'Codex effort catalog', exact: true }).findElement();
  (effort as HTMLElement).focus();
  await userEvent.keyboard('{Enter}{ArrowDown}{Enter}');
  await expect.element(page.getByRole('textbox', { name: 'Codex effort', exact: true })).toHaveValue('catalog-effort');
  expect(sent.filter(request => request.method === 'PUT')).toHaveLength(0);
  await page.getByRole('button', { name: 'Save Codex defaults' }).click();
  await expect.poll(() => sent.filter(request => request.method === 'PUT').length).toBe(1);
});

it('preserves both provider drafts and focused input across desktop and phone hosts without a write', async () => {
  await page.viewport(1180, 844);
  const { sent, router } = setup();
  const codex = page.getByRole('textbox', { name: 'Codex model', exact: true });
  const claude = page.getByRole('textbox', { name: 'Claude effort', exact: true });
  await codex.fill('codex-draft');
  await claude.fill('claude-draft');
  const original = claude.element();
  await page.viewport(390, 844);
  await expect.element(page.getByRole('button', { name: 'Back to Settings' })).toBeVisible();
  await expect.element(claude).toHaveValue('claude-draft');
  await expect.element(claude).toHaveFocus();
  expect(claude.element()).toBe(original);
  expect(page.getByRole('heading', { name: 'Workers', exact: true }).all()).toHaveLength(1);
  await page.viewport(1180, 844);
  await expect.element(page.getByRole('dialog', { name: 'Settings', exact: true })).toBeVisible();
  await expect.element(codex).toHaveValue('codex-draft');
  await expect.element(claude).toHaveValue('claude-draft');
  expect(claude.element()).toBe(original);
  expect(sent.filter(request => request.method === 'PUT')).toHaveLength(0);
  expect(router.state.location.pathname).toBe('/settings/workers');
});
