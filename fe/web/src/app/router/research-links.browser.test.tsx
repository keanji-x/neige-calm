import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import type { ReportLinkTarget } from '../../../../core/domain/report.ts';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { resolveAppReportLink } from './report-links.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(cleanup);

it('opens a saved same-origin research URL through the real Track route and returns with history Back', async () => {
  await page.viewport(1280, 900);
  const user = userEvent.setup();
  const area = { id: 'a1', name: 'Research', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const track = (id: string) => ({ id, area_id: area.id, title: id, sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 });
  const savedUrl = `${window.location.origin}${APP_BASEPATH}/track/research#b-thesis`;
  const savedLayout = { version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [{ kind: 'table', title: 'Research links', span: 1,
    data: { rows: [{ name: '示例公司的研究', track: savedUrl, event: '2026-10-01' }] },
    columns: [{ key: 'name', label: '研究', format: 'text', digits: 0, linkKey: 'track' }, { key: 'event', label: '下次事件', format: 'text', digits: 0 }],
  }] };
  const report = (id: string) => ({ id: `report-${id}`, track_id: id, kind: 'track-report', title: 'Report', sort: 1,
    deletable: false, created_at: 1, updated_at: 2, payload: { schemaVersion: 3, docRev: 1, summary: id, body: '',
      blocks: id === 'source' ? [{ id: 'table', kind: 'layout', rev: 1, payload: savedLayout }]
        : [{ id: 'b-thesis', kind: 'prose', rev: 1, payload: { markdown: '# Research destination\nSaved thesis.' } }],
    } });
  const requests: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    if (request.path === '/api/areas') return Promise.resolve(ok([area]));
    if (request.path === '/api/areas/a1/tracks') return Promise.resolve(ok([track('source'), track('research')]));
    for (const id of ['source', 'research']) {
      if (request.path === `/api/tracks/${id}`) return Promise.resolve(ok({ track: track(id), can_resume: false, cards: [report(id)], overlays: [] }));
      if (request.path === `/api/tracks/${id}/report`) return Promise.resolve(ok({ taskDiagnostics: [] }));
    }
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), client,
    cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/source'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router}/>
  </ThemeProvider></QueryClientProvider>);
  await user.click(await screen.findByRole('button', { name: '示例公司的研究' }, { timeout: 5_000 }));
  await waitFor(() => expect(requests.map(request => request.path)).toContain('/api/tracks/research'));
  expect(router.state.location.href).toBe('/track/research#b-thesis');
  expect(await screen.findByRole('heading', { name: 'Research destination' })).toBeTruthy();
  await page.screenshot({ path: '../../../../test-results/research-link-destination.png' });
  await act(async () => { router.history.back(); await Promise.resolve(); });
  expect(await screen.findByRole('button', { name: '示例公司的研究' })).toBeTruthy();
  expect(screen.getByRole('cell', { name: '2026-10-01' })).toBeTruthy();
  expect(savedLayout.items[0].data.rows[0].track).toBe(savedUrl);
  expect(requests.filter(request => request.method !== 'GET')).toHaveLength(0);
});


it('renders legacy ids and wave citations as links while rejected app URLs remain plain table cells', async () => {
  const user = userEvent.setup();
  const onOpenLink = vi.fn<(target: ReportLinkTarget) => void>();
  const rows = [
    { name: 'Legacy research', target: 'legacy%2Fid' },
    { name: 'Wave research', target: 'neige://wave/study#b-thesis' },
    { name: 'Relative research', target: `${APP_BASEPATH}/track/study%252F1#b%2Dthesis` },
    { name: 'External research', target: 'https://elsewhere.example/next/track/study' },
    { name: 'Wrong page', target: `${window.location.origin}${APP_BASEPATH}/area/study` },
    { name: 'Unsupported protocol', target: 'neige://track/study' },
  ];
  render(<ReportDocument empty={null} onOpenLink={onOpenLink}
    resolveAppLink={destination => resolveAppReportLink(destination, { origin: window.location.origin, basePath: APP_BASEPATH })}
    report={{ summary: '', body: '', blocks: [{ id: 'table', kind: 'layout', payload: {
      version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [{ kind: 'table', title: 'Research', span: 1, data: { rows },
        columns: [{ key: 'name', label: 'Research', format: 'text', digits: 0, linkKey: 'target' }],
      }],
    } }] }}/>
  );
  for (const label of ['Legacy research', 'Wave research', 'Relative research']) await user.click(screen.getByRole('button', { name: label }));
  expect(onOpenLink.mock.calls.map(call => call[0])).toEqual([
    { trackId: 'legacy%2Fid', blockId: null }, { trackId: 'study', blockId: 'b-thesis' }, { trackId: 'study%2F1', blockId: 'b-thesis' },
  ]);
  for (const label of ['External research', 'Wrong page', 'Unsupported protocol']) {
    expect(screen.getByRole('cell', { name: label })).toBeTruthy();
    expect(screen.queryByRole('button', { name: label })).toBeNull();
  }
});
