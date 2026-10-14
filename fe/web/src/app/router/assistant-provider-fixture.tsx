import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { render } from '@testing-library/react';
import type { AgentProvider } from '../../../../core/api/generated/wire.ts';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import type { ManagedProviderAvailability } from '../../../../core/domain/agent-providers.ts';
import { trackConversationCardId, type Conversation } from '../../../../core/domain/conversation.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter, APP_BASEPATH } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

/** Real router/query/draft/store over explicit server declarations, without CLI credentials. */
export function renderAssistantProviderFixture(options: {
  answers: ManagedProviderAvailability[];
  failFirstCreate?: boolean;
  landFirstCreate?: boolean;
  hideLanded?: boolean;
  existingProvider?: AgentProvider;
  dormant?: boolean;
  assistantProviders?: boolean;
  mobile?: boolean;
  webCompatVersion?: number;
  catalogUnavailable?: boolean;
  runAttachmentsSupported?: boolean;
}) {
  const requests: ApiRequest[] = [];
  let attempts = 0;
  let restarted = false;
  let selectedModel: string | null = null;
  const track = { id: 'provider-track', area_id: 'area', title: 'Provider discussion', sort: 1,
    cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const rows: Conversation[] = options.existingProvider === undefined ? [] : [{
    id: 'dormant', trackId: track.id, title: 'Saved Assistant', kind: 'track-assistant',
    provider: options.existingProvider, state: null, updatedAt: 2, lastTurnCompletedAt: null,
  }];
  const catalog = { models: [{ id: 'selected', model: 'selected', display_name: 'Selected model', resolved_model: null,
    description: '', is_default: false, default_reasoning_effort: 'high',
    supported_reasoning_efforts: [{ reasoning_effort: 'low', description: 'Quick' }, { reasoning_effort: 'high', description: 'Thorough' }] }],
    source: 'live', fetched_at_ms: 1, default_source: 'unknown',
    default: { model: null, reasoning_effort: null, supported_reasoning_efforts: null } };
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    let body: unknown = [];
    let status = 200;
    if (request.path === '/api/version') body = { assistantProviders: options.assistantProviders ?? true,
      conversationCreateModel: true, conversationSide: true, webCompatVersion: options.webCompatVersion ?? 53, minWebCompatVersion: options.webCompatVersion ?? 53,
      syncEventVersion: 20, dbInstanceId: 'provider-browser' };
    if (request.path.startsWith('/api/agent-providers/managed')) body = options.answers;
    if (request.path.startsWith('/api/models?')) body = options.catalogUnavailable ? { ...catalog, models: [], source: 'unavailable' } : catalog;
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = [track];
    if (request.path === '/api/settings') body = {};
    if (request.path === `/api/tracks/${track.id}`) body = { track, can_reopen: false, can_close: true,
      cards: rows.map(row => ({ id: row.id, track_id: track.id, title: row.title, kind: 'codex', sort: 1,
        payload: { harness_profile: 'assistant', assistant_provider: row.provider }, deletable: false, created_at: 1, updated_at: 2 })), overlays: [] };
    if (request.path === `/api/tracks/${track.id}/conversations`) {
      if (request.method === 'POST') {
        attempts += 1;
        if (options.failFirstCreate && !options.landFirstCreate && attempts === 1) return { status: 503, statusText: 'Unavailable', body: { code: 'service_unavailable', error: 'Unconfirmed' } };
        const input = request.body as { provider: AgentProvider; side?: { source_card_id: string } };
        const created: Conversation = { id: trackConversationCardId(track.id, request.headers?.['Idempotency-Key'] ?? ''),
          trackId: track.id, provider: input.provider, title: 'Created Assistant', kind: 'track-assistant',
          state: null, updatedAt: 3, lastTurnCompletedAt: null,
          ...(input.side === undefined ? {} : { sourceCardId: input.side.source_card_id }) };
        rows.push(created); body = created; status = 201;
        if (options.landFirstCreate && attempts === 1) return { status: 503, statusText: 'Unavailable', body: { code: 'service_unavailable', error: 'Unconfirmed' } };
      } else body = options.hideLanded ? [] : rows;
    }
    const cardId = request.path.split('/')[3];
    if (request.path.endsWith('/planner/run')) body = { card_id: cardId, worker_session_id: restarted ? 'replacement' : null,
      phase: 'idle', model: selectedModel, reasoning_effort: null, permission_mode: null, blocked_reason: null,
      attachments_supported: options.runAttachmentsSupported ?? false, manual_compact_supported: false, compact_outcome: null,
      context_usage_invalidated: false, running_turn: null };
    if (request.path.endsWith('/planner/model')) {
      const input = request.body as { model: string | null; reasoning_effort: string | null };
      selectedModel = input.model;
      body = { card_id: cardId, ...input, effort_adjusted: false, unknown_model: false };
    }
    if (request.path.endsWith('/planner/input')) {
      if (options.dormant && !restarted) return { status: 409, statusText: 'Conflict', body: {
        code: 'planner_harness_dormant', error: 'Start a fresh session' } };
      body = { card_id: cardId, worker_session_id: 'replacement' };
    }
    if (request.path.endsWith('/planner/restart')) {
      restarted = true;
      body = { card_id: cardId, terminal_id: '', new_thread_id: 'replacement-thread' };
    }
    if (request.path.includes('/harness/items') && cardId === 'dormant') body = [{ id: 1, worker_session_id: 'old',
      card_id: cardId, track_id: track.id, thread_id: 'old-thread', turn_id: 'last-turn', turn_error_text: null,
      item_uuid: null, item_type: 'agentMessage', method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { id: 'reply', type: 'agentMessage', text: 'Saved reply' } }) }];
    return { status, statusText: 'OK', body };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}/track/${track.id}${options.mobile ? "" : "?panel=conversations"}`] }) });
  const container = document.createElement('div'); container.id = 'root'; document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>, { container });
  return { requests, client, rows };
}
