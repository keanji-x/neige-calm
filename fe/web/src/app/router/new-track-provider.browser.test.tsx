// The grouped model picker on the real new-track route, in a real engine: a Claude pick sets the
// Planner's provider and model, drops a Codex effort and offers its own levels (#1822), and the row still fits a phone (#1810).
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
  models: [{ id: 'fast', model: 'gpt-5', resolved_model: null, display_name: 'GPT-5', description: '', is_default: true,
    default_reasoning_effort: 'low', supported_reasoning_efforts: [
      { reasoning_effort: 'low', description: 'Answers sooner' },
      { reasoning_effort: 'high', description: 'Thinks longer' },
    ] }],
  default: { model: null, reasoning_effort: null, supported_reasoning_efforts: null }, default_source: 'unknown', source: 'live', fetched_at_ms: 1,
};
/* What `routes/models.rs` answers for `?provider=claude` (#1822): the Claude CLI's own list, and nothing on a
   server without Claude Planners. */
const LEVELS = ['low', 'medium', 'high', 'xhigh', 'max'].map((level) => ({ reasoning_effort: level, description: null }));
const CLAUDE_CATALOG = {
  models: [
    ['opus[1m]', 'claude-opus-5-5[1m]', 'Opus (1M context)', LEVELS],
    ['claude-fable-5-1[1m]', 'claude-fable-5-1', 'Fable', LEVELS],
    ['sonnet', 'claude-sonnet-5', 'Sonnet', LEVELS],
    ['haiku', 'claude-haiku-4-5-20251001', 'Haiku', []],
  ].map(([value, resolved, name, levels]) => ({
    id: value, model: value, resolved_model: resolved, display_name: name, description: '', is_default: false,
    supported_reasoning_efforts: levels, default_reasoning_effort: null,
  })),
  default: { model: 'claude-opus-5-5[1m]', reasoning_effort: null, supported_reasoning_efforts: LEVELS },
  default_source: 'claude_cli', source: 'live', fetched_at_ms: 1,
};
const NO_CLAUDE = {
  ...CLAUDE_CATALOG, models: [], source: 'unavailable', fetched_at_ms: null,
  default: { model: null, reasoning_effort: null, supported_reasoning_efforts: null }, default_source: 'unknown',
};

/** What `routes/agent_providers.rs` answers alongside each catalog (#1817). */
function availability(claude: 'ready' | 'not_configured') {
  return [
    { provider: 'codex', status: 'ready', reason: null, authentication_notice: null, checked_at_ms: 1 },
    claude === 'ready'
      ? { provider: 'claude', status: 'ready', reason: null, authentication_notice: null, checked_at_ms: 1 }
      : { provider: 'claude', status: 'not_configured', reason: 'calm-server was started without --claude-planner-config', authentication_notice: null, checked_at_ms: 1 },
  ];
}

function mount(claude: unknown) {
  const creates: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    if (request.method === 'POST' && request.path === '/api/tracks') {
      creates.push(request);
      /* Held: the page stays put, so the request is all there is to read. */
      return new Promise<ApiTransportResponse>(() => undefined);
    }
    if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
    if (request.path === '/api/models?provider=codex') return Promise.resolve(ok(LIVE_CATALOG));
    if (request.path === '/api/models?provider=claude') return Promise.resolve(ok(claude));
    if (request.path === '/api/agent-providers') return Promise.resolve(ok(availability(claude === NO_CLAUDE ? 'not_configured' : 'ready')));
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

/** Open the model menu by keyboard: astryx swallows a trigger click that lands right after its menu hid. */
async function openModelMenu(name: string) {
  (await page.getByRole('button', { name }).findElement()).focus();
  await userEvent.keyboard('{ArrowDown}');
}

it.each([1280, 390])('a Claude pick sets provider and model, drops the Codex effort and offers its own (%ipx)', async (width) => {
  await page.viewport(width, 844);
  const { creates } = mount(CLAUDE_CATALOG);
  await openModelMenu('Model: Codex Default');
  await page.getByRole('group', { name: 'Codex' }).getByRole('menuitem', { name: 'GPT-5' }).click();
  if (width < 600) {
    /* Phone: effort lives inside the model menu. */
    await openModelMenu('Model: Codex GPT-5');
  } else {
    await page.getByRole('button', { name: 'Reasoning effort: low (the default)' }).click();
  }
  await page.getByRole('menuitem', { name: /high/ }).click();

  await openModelMenu('Model: Codex GPT-5');
  await page.getByRole('group', { name: 'Claude' }).getByRole('menuitem', { name: 'Haiku' }).click();
  await expect.element(page.getByRole('button', { name: 'Model: Claude Haiku' })).toBeVisible();
  /* Haiku declares no effort level, so there is none to pick. */
  await expect.element(page.getByRole('button', { name: /^Reasoning effort:/ })).not.toBeInTheDocument();

  await openModelMenu('Model: Claude Haiku');
  await page.getByRole('group', { name: 'Claude' }).getByRole('menuitem', { name: 'Fable' }).click();
  await expect.element(page.getByRole('button', { name: 'Model: Claude Fable' })).toBeVisible();
  if (width < 600) {
    await openModelMenu('Model: Claude Fable');
  } else {
    /* The Codex effort did not come along: Fable's own levels start at its default. */
    await page.getByRole('button', { name: 'Reasoning effort: Default' }).click();
  }
  await expect.element(page.getByRole('menuitem', { name: 'xhigh' })).toBeVisible();
  await page.getByRole('menuitem', { name: 'high', exact: true }).click();

  /* The picker and Create share one row, with no horizontal scroll. */
  const model = (await page.getByRole('button', { name: 'Model: Claude Fable' }).findElement()).getBoundingClientRect();
  const send = (await page.getByRole('button', { name: 'Create track' }).findElement()).getBoundingClientRect();
  expect(Math.abs(model.top + model.height / 2 - send.top - send.height / 2)).toBeLessThan(2);
  expect(model.right).toBeLessThanOrEqual(send.left);
  expect(send.right).toBeLessThanOrEqual(width);
  expect(document.documentElement.scrollWidth).toBe(width);

  /* `combobox`: the route attaches the `@` menu, and a field with a trigger takes that role. */
  await page.getByRole('combobox', { name: 'What this track should do' }).fill('Plan with Claude');
  await page.getByRole('button', { name: 'Create track' }).click();
  await expect.poll(() => creates.length).toBe(1);
  expect(creates[0]?.body).toMatchObject({
    planner_provider: 'claude', model: 'claude-fable-5-1[1m]', reasoning_effort: 'high', first_message: 'Plan with Claude',
  });
});

it('offers only Codex on a server without Claude Planners', async () => {
  mount(NO_CLAUDE);
  await openModelMenu('Model: Default');
  await expect.element(page.getByRole('menuitem', { name: 'GPT-5' })).toBeVisible();
  await expect.element(page.getByRole('group', { name: 'Claude' })).not.toBeInTheDocument();
  await expect.element(page.getByRole('menuitem', { name: 'Sonnet' })).not.toBeInTheDocument();
});
