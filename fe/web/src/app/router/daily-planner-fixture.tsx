// Test-only transport for the complete daily homepage composition; no model or live API.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render } from '@testing-library/react';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { trackReportLinkUrl } from '../../../../core/domain/report.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

export function renderDailyFixture({ initial = '/', failChanges = false }: Readonly<{ initial?: string; failChanges?: boolean }> = {}) {
  const requests: ApiRequest[] = [];
  const area = { id: 'daily-area', name: 'system', color: '#6574cd', sort: 0, kind: 'system', created_at: 1, updated_at: 1 };
  const projectArea = { ...area, id: 'project-area', name: 'Project', kind: 'user' };
  const track = { id: 'daily', area_id: area.id, title: 'Renamed daily Track', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const project = { ...track, id: 'project', area_id: projectArea.id, title: 'Project evidence' };
  const card = { id: 'daily-planner', track_id: track.id, kind: 'codex', title: 'Daily Planner conversation', sort: 1, payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 };
  const report = { id: 'daily-report', track_id: track.id, kind: 'track-report', title: null, sort: -1,
    payload: { schemaVersion: 3, docRev: 0, summary: '', body: `# 今日计划\n\nPrioritize the release. Read [Project evidence](${trackReportLinkUrl(project.id)}).\n` }, deletable: false, created_at: 1, updated_at: 2 };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send: async (request) => {
    await Promise.resolve();
    requests.push(request);
    if (request.path === '/api/areas') return ok([projectArea]);
    if (request.path === '/api/areas/daily-area/tracks') return ok([track]);
    if (request.path === '/api/areas/project-area/tracks') return ok([project]);
    if (request.path.startsWith('/api/today/daily')) {
      const date = new URL(request.path, 'http://fixture').searchParams.get('date');
      return ok(date === null || date === '2026-10-04' ? { date: '2026-10-04', time_zone: 'Asia/Shanghai', track_id: track.id } : null);
    }
    if (request.path === '/api/tracks/daily') return ok({ track, can_reopen: false, can_close: false, cards: [card, report], overlays: [] });
    if (request.path === '/api/tracks/project') return ok({ track: project, can_reopen: false, can_close: true, cards: [{ ...report, id: 'project-report', track_id: project.id, payload: { schemaVersion: 3, docRev: 0, summary: '', body: '# Project result\n\nRelease is ready.\n' } }], overlays: [] });
    if (request.path.startsWith('/api/today/report-changes')) return failChanges
      ? { status: 500, statusText: 'Error', body: { error: 'Report history unavailable' } }
      : ok({ date: '2026-10-03', time_zone: 'Asia/Shanghai', through_event_id: 42, next_cursor: null, changes: [{ track_id: project.id, track_title: project.title, area_id: projectArea.id, area_name: projectArea.name, edit_count: 2, first_event_id: 10, last_event_id: 42, summary_before: 'In progress', summary_after: 'Ready', patch: '--- a/report.md\n+++ b/report.md\n@@ -1 +1 @@\n-Building\n+Release is ready.\n', patch_truncated: false }] });
    if (request.path.startsWith('/api/today/report-edits')) return ok({ next_cursor: null, edits: [{ event_id: 42, at: Date.parse('2026-10-03T01:00:00Z'), edit: { track_id: project.id, edit_id: 'edit-42', summary_before: 'In progress', summary_after: 'Ready', body_before: '# Before release\n\nBuilding', body_after: '# After release\n\nRelease is ready.' } }] });
    if (request.path.endsWith('/planner/run')) return ok({ card_id: card.id, worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null, pending_queue: [], running_turn: null, final_reply: null });
    if (request.path.includes('/harness/items')) return ok([]);
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  } };
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [initial] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return { router, requests };
}
