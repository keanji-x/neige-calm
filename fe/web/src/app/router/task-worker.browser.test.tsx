import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { withReadReceipts } from '../providers/read-receipts-fixture.tsx';
import { createAppRouter } from './public.tsx';
import { historicalConversationPage, historicalConversationRun } from './conversation-read-data.ts';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import '../../styles/entry.css';

afterEach(cleanup);

it.each([1080, 390])('reads a terminal-free worker at %i px without human input controls', async width => {
  await page.viewport(width, 844);
  const requests: ApiRequest[] = [];
  const area = { id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const track = { id: 'track', area_id: 'area', title: 'Worker review', sort: 1, cwd: '/tmp', pinned_at: null,
    closed_at: null, created_at: 1, updated_at: 1, workspace: { isolation: 'none', kind: 'attached', path: '/tmp', frozen_at: null } };
  const worker = { id: 'worker', track_id: 'track', kind: 'codex', title: 'Task worker', sort: 1,
    payload: { harness_profile: 'task_worker', worker_provider: 'opencode' }, deletable: true, created_at: 1, updated_at: 1 };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    if (request.path === '/api/areas') return ok([area]);
    if (request.path === '/api/areas/area/tracks') return ok([track]);
    if (request.path === '/api/tracks/track') return ok({ track, planner_revision: 0, can_reopen: false, can_admin: false,
      can_close: true, cards: [worker], overlays: [] });
    if (request.path.startsWith('/api/cards/worker/harness/items?')) return ok(historicalConversationPage('worker', [{
      id: 1, worker_session_id: 'runtime', card_id: 'worker', track_id: 'track', thread_id: 'thread', turn_id: 'turn',
      turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { type: 'agentMessage', text: 'Worker conversation evidence' } }), created_at_ms: 1,
    }], null));
    if (request.path.startsWith('/api/cards/worker/planner/run?')) return ok({ ...historicalConversationRun({
      card_id: 'worker', worker_session_id: 'runtime', phase: 'turn_running', model: null, reasoning_effort: null,
      permission_mode: null, blocked_reason: null, manual_compact_supported: false, recovery: null,
      compact_outcome: null, context_usage_invalidated: false, running_turn: null,
    }, 'running', false), input_contract: 'canonical' });
    if (request.path.startsWith('/api/cards/worker/harness/live?')) return ok({ contract: 'conversation_v1',
      turn: { thread_id: 'thread', turn_id: 'live', items: [{ item_id: 'item', text: 'Streaming worker evidence' }] } });
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport: withReadReceipts(transport), client,
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined, cards: bootTestCardRuntime() });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/track?card=worker'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  await expect.element(page.getByText('Worker conversation evidence')).toBeVisible();
  await expect.element(page.getByText('Streaming worker evidence')).toBeVisible();
  expect(document.querySelector('[data-nc-composer], [data-nc-chat-dock]')).toBeNull();
  await page.screenshot({ path: `__screenshots__/task-worker-${width}.png` });
  expect(requests.filter(request => request.method !== 'GET')).toEqual([]);
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  await expect.poll(() => document.querySelector('#conversation-worker')).toBeNull();
  client.clear();
});
