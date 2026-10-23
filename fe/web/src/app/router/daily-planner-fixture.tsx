import type { SessionIdentity } from '../../../../core/api/auth.ts';
import { SessionGate } from '../auth/session-gate.tsx';
import { historicalConversationPage, conversationCardId, historicalConversationRun } from './conversation-read-data.ts';
import { prepareReportDocument, type ReportPreparationPort } from '../../features/report/document/preparation.ts';
import type { AgentProvider } from '../../../../core/api/generated/wire.ts';
import { Profiler, StrictMode, type ReactNode } from 'react';
// Test-only transport for the complete daily homepage composition; no model or live API.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory, type AnyRoute } from '@tanstack/react-router';
import { render } from '@testing-library/react';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { trackReportLinkUrl } from '../../../../core/domain/report.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import type { UiPreferences } from '../providers/ui-preferences.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { withReadReceipts, type ReadReceiptsKernel } from '../providers/read-receipts-fixture.tsx';

export function renderDailyFixture({ identity, routeProbe, withTransport, strict = false, initial = '/', plannerProvider = 'codex', failChanges = false, emptyWorkspace = false, extraPlanner = false, projectTitle = 'Project evidence', reportBody, reply, onCommit, uiPreferences, receipts, prepareReport = prepareReportDocument }: Readonly<{ identity?: SessionIdentity; routeProbe?: ReactNode; withTransport?: (transport: ApiTransportPort) => ApiTransportPort; strict?: boolean; prepareReport?: ReportPreparationPort; plannerProvider?: AgentProvider; uiPreferences?: UiPreferences; receipts?: ReadReceiptsKernel; initial?: string; failChanges?: boolean; emptyWorkspace?: boolean; extraPlanner?: boolean; projectTitle?: string; reportBody?: string; reply?: (request: ApiRequest) => ApiTransportResponse | undefined | Promise<ApiTransportResponse | undefined>; onCommit?: () => void }> = {}) {
  const requests: ApiRequest[] = [];
  const area = { id: 'daily-area', name: 'system', color: '#6574cd', sort: 0, kind: 'system', created_at: 1, updated_at: 1 };
  const projectArea = { ...area, id: 'project-area', name: 'Project', kind: 'user' };
  const track = { workspace: { isolation: 'managed' as const, kind: 'managed' as const, path: '/tmp', frozen_at: null }, id: 'daily', area_id: area.id, title: 'Renamed daily Track', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const project = { ...track, id: 'project', area_id: projectArea.id, title: projectTitle };
  const card = { id: 'daily-planner', track_id: track.id, kind: 'codex', title: 'Daily Planner conversation', sort: 1, payload: { planner_harness: true, planner_provider: plannerProvider }, deletable: false, created_at: 1, updated_at: 2 };
  const report = { id: 'daily-report', track_id: track.id, kind: 'track-report', title: null, sort: -1,
    payload: { schemaVersion: 3, docRev: 0, summary: '', body: reportBody ?? `# 今日计划\n\nPrioritize the release. Read [Project evidence](${trackReportLinkUrl(project.id)}).\n` }, deletable: false, created_at: 1, updated_at: 2 };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send: async (request) => {
    await Promise.resolve();
    requests.push(request);
    const response = await reply?.(request);
    if (response !== undefined) return response;
    if (request.path === '/api/auth/whoami' && identity !== undefined) return ok(identity);
    if (request.path === '/api/areas') return ok(emptyWorkspace ? [] : [projectArea]);
    if (request.path === '/api/areas/daily-area/tracks') return ok([track]);
    if (request.path === '/api/areas/project-area/tracks') return ok([project]);
    if (request.path.startsWith('/api/today/daily')) {
      const date = new URL(request.path, 'http://fixture').searchParams.get('date');
      return ok(date === null || date === '2026-10-04' ? { date: '2026-10-04', time_zone: 'Asia/Shanghai', track_id: track.id } : null);
    }
    if (request.path === '/api/tracks/daily') return ok({ track, planner_revision: 0, can_reopen: false, can_admin: false, can_close: false, cards: [card, ...(extraPlanner ? [{ ...card, id: 'second-planner', title: 'Other conversation', payload: { harness_profile: 'assistant', assistant_provider: plannerProvider }, deletable: false }] : []), report], overlays: [] });
    if (request.path === '/api/tracks/daily/conversations') return ok(extraPlanner ? [{ id: 'second-planner', trackId: 'daily', title: 'Other conversation', kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null }] : []);
    if (request.path === '/api/tracks/project') return ok({ track: project, planner_revision: 0, can_reopen: false, can_admin: false, can_close: true, cards: [{ ...report, id: 'project-report', track_id: project.id, payload: { schemaVersion: 3, docRev: 0, summary: '', body: '# Project result\n\nRelease is ready.\n' } }], overlays: [] });
    if (request.path.startsWith('/api/today/report-changes')) return failChanges
      ? { status: 500, statusText: 'Error', body: { error: 'Report history unavailable' } }
      : ok({ date: '2026-10-03', timezone: 'Asia/Shanghai', through_event_id: 42, next_cursor: null, changes: [{ track_id: project.id, track_title: project.title, area_id: projectArea.id, area_name: projectArea.name, edit_count: 2, first_event_id: 10, last_event_id: 42, summary_before: 'In progress', summary_after: 'Ready', patch: '--- a/report.md\n+++ b/report.md\n@@ -1 +1 @@\n-Building\n+Release is ready.\n', patch_truncated: false }] });
    if (request.path.startsWith('/api/today/report-edits')) return ok({ next_cursor: null, edits: [{ event_id: 42, at: Date.parse('2026-10-03T01:00:00Z'), edit: { track_id: project.id, edit_id: 'edit-42', summary_before: 'In progress', summary_after: 'Ready', body_before: '# Before release\n\nBuilding', body_after: '# After release\n\nRelease is ready.' } }] });
    if (request.path.includes('/planner/run?contract=conversation_v1')) return ok(historicalConversationRun({ card_id: card.id, worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, pending_queue: [], manual_compact_supported: true, recovery: null, compact_outcome: null, context_usage_invalidated: false, running_turn: null, final_reply: null }, 'idle', true));
    if (request.path.includes('/harness/items')) return ok(historicalConversationPage(conversationCardId(request.path), [], null));
    if (request.path === '/api/plugins') return ok([{ id: 'calendar', version: '0.1.0', enabled: true, state: 'running', manifest_name: 'Calendar', has_config: false, can_uninstall: false, can_disable: false }]);
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  } };
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const fixtureTransport = withReadReceipts(transport, receipts);
  const router = createAppRouter({ prepareReport, transport: withTransport?.(fixtureTransport) ?? fixtureTransport, unauthorized, client, cards: bootTestCardRuntime(), uiPreferences, onSignOut: () => undefined });
  // Observe commits inside the existing provider via declared child routes. Each
  // wrapper delegates the original component and adds only a nonvisual test probe.
  if (routeProbe !== undefined) for (const route of (router.routeTree.children ?? []) as readonly AnyRoute[]) {
    const Component = route.options.component;
    if (Component !== undefined) route.update({ component: () => <><Component />{routeProbe}</> });
  }
  router.update({ history: createMemoryHistory({ initialEntries: [initial] }) });
  const container = document.createElement('div');
  container.id = 'root';
  document.body.appendChild(container);
  const content=<RouterProvider router={router}/>;
  const authenticated=identity === undefined ? content : <SessionGate transport={fixtureTransport} unauthorized={unauthorized} client={client}
    runtime={{deleteDatabase:()=>undefined,idbDatabaseName:'fixture'}} cursorStore={{clear:()=>undefined}}
    renderLogin={()=><p>Sign in required</p>} renderError={()=><p>Identity unavailable</p>}>{content}</SessionGate>;
  const tree = <Profiler id="daily-fixture" onRender={onCommit ?? (() => undefined)}><QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>{authenticated}</ThemeProvider></QueryClientProvider></Profiler>;
  render(strict ? <StrictMode>{tree}</StrictMode> : tree, { container });
  return { router, requests, client };
}
