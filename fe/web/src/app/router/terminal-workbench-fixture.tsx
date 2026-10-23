import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { TerminalPresentation } from '../../../../core/domain/terminal-presentation.ts';
import type { CardWire } from '../../../../core/domain/track.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { createReadReceiptsKernel } from '../providers/read-receipts-fixture.tsx';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter, APP_BASEPATH } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { conversationCardId, historicalConversationPage, historicalConversationRun } from './conversation-read-data.ts';

export function terminalCard(id: string, presentation: TerminalPresentation, trackId = 'workbench'): CardWire {
  return { id, track_id: trackId, kind: 'terminal', title: id, sort: 1, payload: { terminal_presentation: presentation }, deletable: true, created_at: 1, updated_at: 2 };
}

/** HTTP boundary fixture; production router, mutations, card registry and lifecycle remain real. */
export function setupWorkbench({ cards: initial = [] as CardWire[], defer = false, lost = false, daily = false, failure = null as null | 'definite' | 'unknown', initialCard = null as string | null, secondaryReads = null as Promise<void> | null, createdTerminalId = null as string | null } = {}) {
  const requests: ApiRequest[] = [];
  const receipts = createReadReceiptsKernel({ baselineMs: 0 });
  const cards = [...initial];
  const otherCards: CardWire[] = [];
  const live = { text: '' };
  const releases: (() => void)[] = [];
  const keys = new Map<string, CardWire>();
  const track = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: 'workbench', area_id: 'area', title: 'Workbench track', sort: 1,
    cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const parent = { id: 'parent', trackId: track.id, title: 'Main discussion', archivedAt: null, kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, lastTurnCompletedAt: null };
  const rows = [parent];
  let lose = lost;
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { async send(request) {
    requests.push(request);
    const receipt = await receipts.reply(request);
    if (receipt !== undefined) return receipt;
    const { path, method } = request;
    if (path === '/api/today/daily') return ok({ date: '2026-10-17', time_zone: 'Asia/Shanghai', track_id: track.id });
    if (path === '/api/version') return ok({ assistantProviders: true, conversationSide: true, webCompatVersion: WEB_COMPAT_VERSION,
      minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 1, dbInstanceId: 'workbench-tests' });
    if (path.startsWith('/api/agent-providers/managed')) return ok([{ availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      accepts_unavailable_blank_track: true, capabilities: { managed_conversation: true, steer: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false } }]);
    if (path === '/api/areas') return ok([{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }]);
    if (path === '/api/areas/area/tracks') return ok([track]);
    if (path === '/api/settings') return ok({});
    if (path === '/api/tracks/other') return ok({ track: { ...track, id: 'other', title: 'Other track' }, can_reopen: false, can_admin: false, can_close: true, cards: [...otherCards], overlays: [] });
    if (path === '/api/tracks/workbench') return ok({ track, can_reopen: false, can_admin: false, can_close: true, cards: [...cards], overlays: [] });
    if (secondaryReads !== null && ['/api/tracks/workbench/report', '/api/tracks/workbench/conversations', '/api/tracks/workbench/backlinks'].includes(path)) await secondaryReads;
    if (path === '/api/tracks/workbench/report') return ok({ taskDiagnostics: [] });
    if (path === '/api/tracks/workbench/conversations') return ok(rows);
    if (path === '/api/tracks/other/terminal-cards' && method === 'POST') {
      const card = terminalCard('other-terminal', { source: 'manual', in_workbench: true }, 'other'); otherCards.push(card); return ok(card);
    }
    if (path === '/api/tracks/workbench/terminal-cards' && method === 'POST') {
      if (defer) await new Promise<void>(resolve => releases.push(resolve));
      const outcome = failure; failure = null;
      if (outcome === 'definite') return { status: 400, statusText: 'Bad Request', body: { code: 'bad_request', error: 'Terminal refused' } };
      const key = request.headers?.['Idempotency-Key'] ?? '';
      let card = keys.get(key);
      if (card === undefined) { card = terminalCard(`terminal-${keys.size + 1}`, { source: 'manual', in_workbench: true }); if (createdTerminalId !== null) card = { ...card, payload: { ...(card.payload as Record<string, unknown>), terminal_id: createdTerminalId } }; cards.push(card); keys.set(key, card); }
      if (lose || outcome === 'unknown') { lose = false; throw new Error('lost response'); }
      return ok(card);
    }
    if (method === 'PATCH' && path.endsWith('/terminal-workbench')) {
      const id = decodeURIComponent(path.split('/')[3]);
      const index = cards.findIndex(card => card.id === id);
      if (index < 0) return { status: 404, statusText: 'Not Found', body: {} };
      const current = cards[index];
      const payload = current.payload as { terminal_presentation: { source: string; in_workbench: boolean } };
      cards[index] = { ...current, payload: { ...payload, terminal_presentation: {
        ...payload.terminal_presentation, in_workbench: (request.body as { in_workbench: boolean }).in_workbench,
      } } };
      return ok(cards[index]);
    }
    if (method === 'DELETE' && path.startsWith('/api/cards/')) { cards.splice(cards.findIndex(card => card.id === path.split('/')[3]), 1); return ok(undefined); }
    if (path.includes('/planner/run?contract=conversation_v1')) return ok(historicalConversationRun({ card_id: conversationCardId(path), worker_session_id: 'session-parent', phase: live.text === '' ? 'idle' : 'issuing_turn', model: null,
      reasoning_effort: null, permission_mode: null, blocked_reason: null, recovery: null, manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false, running_turn: null }, live.text === '' ? 'idle' : 'running', true));
    if (path.includes('/harness/items?contract=conversation_v1')) return ok(historicalConversationPage(conversationCardId(path), [], null));
    if (path.endsWith('/harness/live')) return ok(live.text === '' ? { turn_id: null, items: [] }
      : { turn_id: 'streamed-turn', items: [{ item_id: 'streamed-message', text: live.text }] });
    return ok([]);
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const booted = bootTestCardRuntime();
  const runtime = { ...booted, host: { ...booted.host } };
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: runtime,
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}${daily ? '/' : '/track/workbench'}${initialCard === null ? '' : `?card=${encodeURIComponent(initialCard)}`}`] }) });
  const container = document.createElement('div'); container.id = 'root'; document.body.append(container);
  const view = render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container });
  return { ...view, requests, releases, router, client, runtime, cards, live, posts: () => requests.filter(request => request.method === 'POST' && request.path.endsWith('/terminal-cards')) };
}


/** Use the public composer command after removal of the redundant header shortcut. */
export async function terminalCommandInput(): Promise<HTMLElement> {
  if (screen.queryAllByRole('combobox', { name: 'Message' }).length === 0) {
    const conversation = screen.queryByRole('button', { name: 'Conversation Main discussion' });
    fireEvent.click(conversation ?? await screen.findByRole('button', { name: /^New conversation$/u }));
  }
  const input = (await screen.findAllByRole('combobox', { name: 'Message' }))[0];
  await waitFor(() => { if (input.getAttribute('contenteditable') !== 'true') throw new Error('Composer is not ready'); });
  return input;
}

export async function openTerminalCommand(): Promise<void> {
  const input = await terminalCommandInput();
  input.focus();
  input.textContent = '/terminal';
  fireEvent.input(input);
  fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
}
