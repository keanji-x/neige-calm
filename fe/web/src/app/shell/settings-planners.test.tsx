import { prepareReportDocument } from '../../features/report/document/preparation.ts';
// @vitest-environment jsdom
// Settings › Planners through the real router and query layer (#1817): the pane reads
// shared availability and managed capabilities; Recheck refreshes only availability.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { RECHECK_TEXT, unavailableCreationNote } from '../../../../core/domain/agent-providers.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { managedProvidersQueryKey } from '../providers/managed-providers.ts';
import { queryKeys } from '../providers/queries.ts';
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
    { availability: { provider: 'codex', status: 'ready', reason: null, authentication_notice: null, checked_at_ms: 1_760_000_000_000 },
      accepts_unavailable_blank_track: true, capabilities: { managed_conversation: true, steer: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false } },
    { availability: { provider: 'claude', ...claude, authentication_notice: null, checked_at_ms: 1_760_000_000_000 },
      accepts_unavailable_blank_track: true, capabilities: { managed_conversation: true, steer: false, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: true } },
  ];
}

/** The managed endpoint wraps the same availability answer as the shell subscription. */
function providerResponse(request: ApiRequest, entries: ReturnType<typeof answer>): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body: request.path.startsWith('/api/agent-providers') && !request.path.includes('/managed')
    ? entries.map(({ availability }) => availability) : entries };
}

/** `recheck` answers `refresh=true` in place of the ready answer. */
function renderPlanners(recheck?: () => Promise<ApiTransportResponse>,
  settingsReply?: (request: ApiRequest) => Promise<ApiTransportResponse>,
  providersReply?: (request: ApiRequest) => Promise<ApiTransportResponse>) {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = {
    send(request): Promise<ApiTransportResponse> {
      sent.push(request);
      if (providersReply !== undefined && request.path.startsWith('/api/agent-providers')) return providersReply(request);
      if (request.path === '/api/agent-providers') return Promise.resolve(providerResponse(request, answer({ status: 'unavailable', reason: LOGGED_OUT })));
      if (settingsReply !== undefined && request.path === '/api/settings') return settingsReply(request);
      if (recheck !== undefined && request.path === '/api/agent-providers?refresh=true') return recheck();
      const body = request.path === '/api/agent-providers/managed'
        ? answer({ status: 'unavailable', reason: LOGGED_OUT })
        : request.path === '/api/agent-providers?refresh=true'
          ? answer({ status: 'ready', reason: null }).map(({ availability }) => availability)
          : request.path === '/api/settings' ? { settings: {} } : [];
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(queryKeys.modelCatalog({ kind: 'provider', provider: 'claude' }), { models: [] });
  const router = createAppRouter({ prepareReport: prepareReportDocument,
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
  return { sent, client };
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

it('lists each provider with its status and reason, and Recheck replaces the answer and refreshes the catalogs', async () => {
  const { sent, client } = renderPlanners();
  const claudeCatalog = queryKeys.modelCatalog({ kind: 'provider', provider: 'claude' });
  await screen.findByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`);
  expect(within(row('Claude')).getByText('Unavailable')).toBeTruthy();
  expect(within(row('Codex')).getByText('Ready')).toBeTruthy();
  expect(sent.filter((request) => request.path === '/api/agent-providers?refresh=true')).toHaveLength(0);
  expect(client.getQueryState(claudeCatalog)?.isInvalidated).toBe(false);
  const readsBeforeRecheck = sent.filter((request) => request.path === '/api/agent-providers/managed').length;

  await userEvent.click(screen.getByRole('button', { name: 'Recheck planners' }));
  await waitFor(() => expect(screen.queryByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`)).toBeNull());
  expect(within(row('Claude')).getByText('Ready')).toBeTruthy();
  expect(sent.filter((request) => request.path === '/api/agent-providers?refresh=true')).toHaveLength(1);
  /* The recheck re-fetched the Claude CLI's model list on the server (#1822). */
  expect(client.getQueryState(claudeCatalog)?.isInvalidated).toBe(true);
  expect(sent.filter((request) => request.path === '/api/agent-providers/managed')).toHaveLength(readsBeforeRecheck);
  expect(client.getQueryData(queryKeys.agentProviders())).toEqual(answer({ status: 'ready', reason: null }).map(({ availability }) => availability));
});

/* #2131 S7: Recheck is a probe; a lost answer shows a fixed sentence, never transport text, and the answer stays. */
describe('a failed Recheck', () => {
  const RAW_OR_CONNECTIVITY = /Transport request failed|timed out|schema|offline|reconnect|connection/i;
  it.each([
    ['a lost answer', () => Promise.reject(new Error('socket hang up')), RECHECK_TEXT.unfinished],
    ['an unreadable answer', () => Promise.resolve({ status: 200, statusText: 'OK', body: { not: 'a list' } }), RECHECK_TEXT.unfinished],
    ['an answered failure', () => Promise.resolve({ status: 500, statusText: 'Server Error', body: { error: 'checks are wedged', code: 'internal' } }),
      'checks are wedged'],
    /* A refusal about one field names it, as every answered reason does (`failureReason`). */
    ['a refusal about one field', () => Promise.resolve({ status: 400, statusText: 'Bad Request',
      body: { error: 'must be true or false', code: 'bad_request', field: 'refresh' } }), 'refresh: must be true or false'],
  ] as const)('shows %s as its sentence and keeps the previous answer', async (_name, reply, text) => {
    renderPlanners(reply);
    await screen.findByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`);
    await userEvent.click(screen.getByRole('button', { name: 'Recheck planners' }));
    expect((await screen.findByRole('alert')).textContent).toBe(text);
    expect(screen.getByRole('alert').textContent).not.toMatch(RAW_OR_CONNECTIVITY);
    expect(screen.getByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`)).toBeTruthy();
  });
});


it('hosts turn duration writes through settings and refetches without clobbering other keys', async () => {
  const { sent } = renderPlanners();
  const field = await screen.findByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  await userEvent.clear(field);
  await userEvent.type(field, '45{Enter}');
  await waitFor(() => expect(sent.filter((request) => request.method === 'PUT')).toHaveLength(1));
  expect(sent.find((request) => request.method === 'PUT')?.body)
    .toEqual({ settings: { planner_max_turn_duration_minutes: '45' } });
  await waitFor(() => expect(sent.filter((request) => request.path === '/api/settings' && request.method === 'GET').length)
    .toBeGreaterThan(1));
});


it('repairs corrupt storage with null and reads the default without rewriting a proxy', async () => {
  let stored: Record<string, string> = { planner_max_turn_duration_minutes: 'broken', http_proxy: 'http://proxy' };
  const { sent } = renderPlanners(undefined, (request) => {
    if (request.method === 'PUT') {
      expect(request.body).toEqual({ settings: { planner_max_turn_duration_minutes: null } });
      stored = { http_proxy: 'http://proxy' };
    }
    return Promise.resolve({ status: 200, statusText: 'OK', body: { settings: stored } });
  });
  const field = await screen.findByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  expect((field as HTMLInputElement).value).toBe('broken');
  expect(field.getAttribute('aria-invalid')).toBe('true');
  const ids = field.getAttribute('aria-describedby')!.split(/\s+/);
  expect(ids.map((id) => document.getElementById(id)?.textContent).join(' ')).toContain('Stored duration is invalid');
  await userEvent.click(screen.getByRole('button', { name: 'Restore default duration' }));
  await waitFor(() => expect(field.getAttribute('aria-invalid')).toBeNull());
  expect((field as HTMLInputElement).value).toBe('30');
  expect(sent.filter((request) => request.method === 'PUT')).toHaveLength(1);
  expect(stored.http_proxy).toBe('http://proxy');
});

it.each([
  ['refused', 400, 'Duration must be between 1 and 240'],
  ['unknown', 500, 'The save is unconfirmed.'],
])('shows a %s settings write, does not retry on blur, and permits an explicit recovery', async (_label, status, sentence) => {
  let minutes = '30';
  let refused = true;
  const { sent } = renderPlanners(undefined, (request) => {
    if (request.method === 'PUT' && refused) return Promise.resolve({ status, statusText: 'Error',
      body: { code: 'bad_request', error: 'Duration must be between 1 and 240' } });
    if (request.method === 'PUT') minutes = '45';
    return Promise.resolve({ status: 200, statusText: 'OK', body: { settings: { planner_max_turn_duration_minutes: minutes } } });
  });
  const field = await screen.findByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  await userEvent.clear(field);
  await userEvent.type(field, '45{Enter}');
  expect((await screen.findByRole('alert')).textContent).toContain(sentence);
  await userEvent.tab();
  expect(sent.filter((request) => request.method === 'PUT')).toHaveLength(1);
  refused = false;
  await userEvent.click(field);
  await userEvent.keyboard('{Enter}');
  await screen.findByText('Saved.');
  expect(sent.filter((request) => request.method === 'PUT')).toHaveLength(2);
});

it('a same-value fresh GET overrides an obsolete PUT acknowledgement even with a frozen clock', async () => {
  const clock = vi.spyOn(Date, 'now').mockReturnValue(1000);
  try {
    const { sent } = renderPlanners(undefined, (request) => Promise.resolve({ status: 200, statusText: 'OK',
      body: { settings: { planner_max_turn_duration_minutes: request.method === 'PUT' ? '45' : '30' } } }));
    const field = await screen.findByRole<HTMLInputElement>('textbox', { name: 'Maximum turn duration (minutes)' });
    await userEvent.clear(field);
    await userEvent.type(field, '45{Enter}');
    await waitFor(() => expect(sent.filter((request) => request.path === '/api/settings' && request.method === 'GET').length).toBeGreaterThan(1));
    await screen.findByText(/Changed elsewhere/);
    expect(screen.queryByText('Saved.')).toBeNull();
    expect(field.value).toBe('45');
    expect(sent.filter((request) => request.method === 'PUT')).toHaveLength(1);
  } finally { clock.mockRestore(); }
});


it('injects blank-creation facts through the route without consulting managed-conversation readiness', async () => {
  const entries = answer({ status: 'unavailable', reason: LOGGED_OUT });
  renderPlanners(undefined, undefined, (request) => Promise.resolve(providerResponse(request, entries)));
  await screen.findByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`);
  expect(within(row('Claude')).getByText('Unavailable')).toBeTruthy();
});

it('retains Checking, first-read retry and pending Recheck states through the route', async () => {
  let release!: (value: ApiTransportResponse) => void;
  let fail = true;
  const { sent } = renderPlanners(undefined, undefined, (request) => {
    if (request.path.endsWith('refresh=true')) return new Promise((resolve) => { release = resolve; });
    if (fail) return Promise.reject(new Error('read failed'));
    return Promise.resolve(providerResponse(request, answer({ status: 'unavailable', reason: LOGGED_OUT })));
  });
  await screen.findByText('Planners are unavailable.');
  expect(screen.queryByText('Checking providers…')).toBeNull();
  fail = false;
  await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
  await screen.findByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`);
  await userEvent.click(screen.getByRole('button', { name: 'Recheck planners' }));
  expect(screen.getByRole('button', { name: 'Recheck planners' }).hasAttribute('disabled')).toBe(true);
  expect(screen.getByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`)).toBeTruthy();
  release({ status: 200, statusText: 'OK', body: answer({ status: 'ready', reason: null }).map(({ availability }) => availability) });
  await waitFor(() => expect(screen.queryByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`)).toBeNull());
  expect(sent.filter((request) => request.path.endsWith('refresh=true'))).toHaveLength(1);
});

it('keeps fresh Recheck availability when an older managed read resolves later', async () => {
  let release!: (value: ApiTransportResponse) => void;
  let hold = false;
  const stale = answer({ status: 'unavailable', reason: LOGGED_OUT });
  const fresh = answer({ status: 'ready', reason: null });
  fresh[0].availability.checked_at_ms += 60_000;
  fresh[1].availability.checked_at_ms += 120_000;
  const { client } = renderPlanners(undefined, undefined, (request) => {
    if (request.path.endsWith('refresh=true')) return Promise.resolve(providerResponse(request, fresh));
    if (hold && request.path === '/api/agent-providers/managed') return new Promise((resolve) => { release = resolve; });
    return Promise.resolve(providerResponse(request, stale));
  });
  await screen.findByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`);
  hold = true;
  const pending = client.refetchQueries({ queryKey: managedProvidersQueryKey });
  await waitFor(() => expect(release).toBeTypeOf('function'));
  await userEvent.click(screen.getByRole('button', { name: 'Recheck planners' }));
  await waitFor(() => expect(screen.queryByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`)).toBeNull());
  const checkedAt = `Last checked at ${new Date(fresh[0].availability.checked_at_ms).toLocaleTimeString()}. The server keeps an answer for 30 seconds.`;
  expect(screen.getByText(checkedAt)).toBeTruthy();
  await act(async () => { release({ status: 200, statusText: 'OK', body: stale }); await pending;
    // React Query batches observer notifications on the next task.
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  expect(within(row('Claude')).getByText('Ready')).toBeTruthy();
  expect(screen.queryByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`)).toBeNull();
  expect(screen.getByText(/^Last checked at /).textContent).toBe(checkedAt);
  expect(client.getQueryData(queryKeys.agentProviders())).toEqual(fresh.map(({ availability }) => availability));
});

it.each(['/api/agent-providers', '/api/agent-providers/managed'])('keeps Checking until %s arrives and joins capabilities by provider', async (delayedPath) => {
  let release!: (value: ApiTransportResponse) => void;
  const live = answer({ status: 'unavailable', reason: LOGGED_OUT });
  const managed = answer({ status: 'ready', reason: null }).reverse();
  renderPlanners(undefined, undefined, (request) => request.path === delayedPath
    ? new Promise((resolve) => { release = resolve; })
    : Promise.resolve(providerResponse(request, request.path === '/api/agent-providers' ? live : managed)));
  await screen.findByText('Checking providers…');
  await waitFor(() => expect(release).toBeTypeOf('function'));
  act(() => { release(providerResponse({ path: delayedPath } as ApiRequest, delayedPath === '/api/agent-providers' ? live : managed)); });
  await screen.findByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`);
  expect(within(row('Claude')).getByText('Unavailable')).toBeTruthy();
  expect(screen.getAllByRole('listitem').indexOf(row('Codex'))).toBeLessThan(screen.getAllByRole('listitem').indexOf(row('Claude')));
});

it('reports missing managed capabilities explicitly and retries both reads', async () => {
  let missing = true;
  const { sent } = renderPlanners(undefined, undefined, (request) => {
    const entries = answer({ status: 'unavailable', reason: LOGGED_OUT });
    return Promise.resolve(providerResponse(request, missing && request.path === '/api/agent-providers/managed' ? entries.slice(0, 1) : entries));
  });
  await screen.findByText('Planner capabilities are unavailable for claude.');
  expect(screen.queryByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`)).toBeNull();
  missing = false;
  await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
  await screen.findByText(`${LOGGED_OUT} ${unavailableCreationNote('Claude')}`);
  expect(sent.filter(({ path }) => path === '/api/agent-providers').length).toBeGreaterThan(1);
  expect(sent.filter(({ path }) => path === '/api/agent-providers/managed').length).toBeGreaterThan(1);
});

it('shares Settings availability with the shell notice through Recheck and Codex retry', async () => {
  let signedIn = false;
  const confirmed = 'Sign in for this server.';
  const { sent, client } = renderPlanners(undefined, undefined, (request) => {
    if (request.method === 'POST') {
      signedIn = true;
      return Promise.resolve({ status: 200, statusText: 'OK', body: {
        status: 'retry_requested', requested_revision: '8', recovery_notices: [],
      } });
    }
    if (request.path.endsWith('refresh=true')) signedIn = true;
    const entries = answer({ status: 'ready', reason: null });
    if (!signedIn) Object.assign(entries[0].availability, { status: 'unavailable', reason: 'outage reason',
      authentication_notice: { kind: 'sign_in_required', text: confirmed, revision: '7' } });
    return Promise.resolve(providerResponse(request, entries));
  });
  await screen.findByRole('button', { name: 'Retry queued messages after server sign-in' });
  expect(screen.getAllByText(confirmed)).toHaveLength(2);
  expect(screen.queryByText(/outage reason/)).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Recheck planners' }));
  await waitFor(() => expect(screen.queryByText(confirmed)).toBeNull());
  expect(screen.queryByRole('button', { name: 'Retry queued messages after server sign-in' })).toBeNull();
  signedIn = false;
  await client.invalidateQueries({ queryKey: queryKeys.agentProviders() });
  await screen.findByRole('button', { name: 'Retry queued messages after server sign-in' });
  await userEvent.click(screen.getByRole('button', { name: 'Retry queued messages after server sign-in' }));
  await waitFor(() => expect(screen.queryByText(confirmed)).toBeNull());
  expect(sent.filter((request) => request.method === 'POST').map((request) => ({ path: request.path, body: request.body })))
    .toEqual([{ path: '/api/agent-providers/codex/retry', body: { expected_revision: '7' } }]);
});
