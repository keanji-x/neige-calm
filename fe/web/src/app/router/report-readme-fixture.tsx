// Real routing/rendering; only the API transport is scripted. File bodies stay
// imported from the repository so tables, links and document length cannot drift.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { render } from '@testing-library/react';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import readme from '../../../../../docs/README.md?raw';
import rootReadme from '../../../../../README.md?raw';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

export const README_TRACK_ID = 'aadd6ed448d349228d672b1546b6a48c';
export const README_TRACK_URL = `/next/track/${README_TRACK_ID}`;
export const README_FILE_URL = `${README_TRACK_URL}?file=docs%2FREADME.md`;

export function renderReadmeFixture(initial: string, detailReady: Promise<void> = Promise.resolve(),
  options: Readonly<{ shortReport?: boolean; failFileRead?: boolean }> = {}) {
  let fileReadFailed = options.failFileRead ?? false;
  const requests: ApiRequest[] = [];
  const track = { id: README_TRACK_ID, area_id: 'project', title: 'Documentation preview', sort: 1,
    cwd: '/repo', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const card = { id: 'planner', track_id: track.id, kind: 'codex', title: 'Existing conversation', sort: 1,
    payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 };
  const report = { ...card, id: 'report', kind: 'track-report', title: null, sort: 0,
    payload: { schemaVersion: 3, docRev: 1, summary: '', body: '', blocks: [{ id: 'documentation', kind: 'prose', rev: 1,
      payload: { markdown: `# Report\n\n${options.shortReport ? '' : readme}\n\nRead [docs/README.md](docs/README.md).` } }] } };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { async send(request) {
    requests.push(request);
    if (request.method !== 'GET') throw new Error(`Unexpected write: ${request.method} ${request.path}`);
    if (request.path === '/api/areas') return ok([{ id: 'project', name: 'Project', color: '#6574cd', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }]);
    if (request.path === '/api/areas/project/tracks') return ok([track]);
    if (request.path === `/api/tracks/${track.id}`) {
      await detailReady;
      return ok({ track, can_reopen: false, can_close: true, cards: [card, report], overlays: [] });
    }
    if (request.path === `/api/tracks/${track.id}/report`) return ok({ taskDiagnostics: [] });
    if (request.path === `/api/tracks/${track.id}/workspace/readfile?path=docs%2FREADME.md`) {
      if (fileReadFailed) return { status: 500, statusText: 'Internal Server Error',
        body: { code: 'internal', message: 'secure workspace reads require Linux openat2 support' } };
      return ok({ path: '/repo/docs/README.md', size: new TextEncoder().encode(readme).length, text: readme, truncated: false });
    }
    if (request.path === `/api/tracks/${track.id}/workspace/readfile?path=README.md`) {
      return ok({ path: '/repo/README.md', size: new TextEncoder().encode(rootReadme).length, text: rootReadme, truncated: false });
    }
    if (request.path.includes('/readfile')) throw new Error(`Unexpected file read: ${request.path}`);
    if (request.path.endsWith('/planner/run')) return ok({ card_id: card.id, worker_session_id: 'runtime', phase: 'idle',
      model: null, reasoning_effort: null, blocked_reason: null, pending_queue: [], running_turn: null, final_reply: null });
    if (request.path.includes('/harness/items')) return ok([{ id: 1, worker_session_id: 'runtime', card_id: card.id, track_id: track.id,
      thread_id: 'thread', turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: 'Conversation remains available while reading documentation.' } }), created_at_ms: 1 }]);
    if (request.path === '/api/settings') return ok({});
    return ok([]);
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [initial] }) });
  const container = document.createElement('div');
  container.id = 'root';
  document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>, { container });
  return { router, requests, client, recoverFileRead: () => { fileReadFailed = false; } };
}
