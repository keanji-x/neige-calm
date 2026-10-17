import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { render } from '@testing-library/react';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { CardWire } from '../../../../core/domain/track.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter, APP_BASEPATH } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

export function terminalCard(id: string, trackId = 'workbench'): CardWire {
  return { id, track_id: trackId, kind: 'terminal', title: id, sort: 1, payload: {}, deletable: true, created_at: 1, updated_at: 2 };
}

/** HTTP boundary fixture; production router, mutations, card registry and lifecycle remain real. */
export function setupWorkbench({ cards: initial = [] as CardWire[], defer = false, lost = false, daily = false, failure = null as null | 'definite' | 'unknown' } = {}) {
  const requests: ApiRequest[] = [];
  const cards = [...initial];
  const releases: (() => void)[] = [];
  const keys = new Map<string, CardWire>();
  const track = { id: 'workbench', area_id: 'area', title: 'Workbench track', sort: 1,
    cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const parent = { id: 'parent', trackId: track.id, title: 'Main discussion', kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, lastTurnCompletedAt: null };
  const rows = [parent];
  let lose = lost;
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { async send(request) {
    requests.push(request);
    const { path, method } = request;
    if (path === '/api/today/daily') return ok({ date: '2026-10-17', time_zone: 'Asia/Shanghai', track_id: track.id });
    if (path === '/api/version') return ok({ assistantProviders: true, conversationSide: true, webCompatVersion: WEB_COMPAT_VERSION,
      minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 1, dbInstanceId: 'workbench-tests' });
    if (path.startsWith('/api/agent-providers/managed')) return ok([{ availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      capabilities: { managed_conversation: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false } }]);
    if (path === '/api/areas') return ok([{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }]);
    if (path === '/api/areas/area/tracks') return ok([track]);
    if (path === '/api/settings') return ok({});
    if (path === '/api/tracks/other') return ok({ track: { ...track, id: 'other', title: 'Other track' }, can_reopen: false, can_admin: false, can_close: true, cards: [], overlays: [] });
    if (path === '/api/tracks/workbench') return ok({ track, can_reopen: false, can_admin: false, can_close: true, cards: [...cards], overlays: [] });
    if (path === '/api/tracks/workbench/report') return ok({ taskDiagnostics: [] });
    if (path === '/api/tracks/workbench/conversations') return ok(rows);
    if (path === '/api/tracks/workbench/terminal-cards' && method === 'POST') {
      if (defer) await new Promise<void>(resolve => releases.push(resolve));
      const outcome = failure; failure = null;
      if (outcome === 'definite') return { status: 400, statusText: 'Bad Request', body: { code: 'bad_request', error: 'Terminal refused' } };
      const key = request.headers?.['Idempotency-Key'] ?? '';
      let card = keys.get(key);
      if (card === undefined) { card = terminalCard(`terminal-${keys.size + 1}`); cards.push(card); keys.set(key, card); }
      if (lose || outcome === 'unknown') { lose = false; throw new Error('lost response'); }
      return ok(card);
    }
    if (method === 'DELETE' && path.startsWith('/api/cards/')) { cards.splice(cards.findIndex(card => card.id === path.split('/')[3]), 1); return ok(undefined); }
    if (path.endsWith('/planner/run')) return ok({ card_id: 'parent', worker_session_id: 'session-parent', phase: 'idle', model: null,
      reasoning_effort: null, permission_mode: null, blocked_reason: null, manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false, running_turn: null });
    if (path.endsWith('/harness/live')) return ok({ turn_id: null, items: [] });
    return ok([]);
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const booted = bootTestCardRuntime();
  const runtime = { ...booted, host: { ...booted.host } };
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: runtime,
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}${daily ? '/' : '/track/workbench'}`] }) });
  const container = document.createElement('div'); container.id = 'root'; document.body.append(container);
  const view = render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container });
  return { ...view, requests, releases, router, client, runtime, cards, posts: () => requests.filter(request => request.method === 'POST' && request.path.endsWith('/terminal-cards')) };
}
