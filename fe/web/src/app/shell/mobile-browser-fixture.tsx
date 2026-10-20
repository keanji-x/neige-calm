import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render } from '@testing-library/react';
import { vi } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from '../router/public.tsx';
import type { ReadReceiptsKernel } from '../providers/read-receipts-fixture.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { DepthFeedback } from '../../ui/motion/depth-feedback.tsx';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';

export const AREA = Object.freeze({ id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 });
export function setup(path: string, areaName: string = AREA.name, onRequest: (request: ApiRequest) => void = () => undefined, receipts?: ReadReceiptsKernel) {
  const OTHER_AREA = { id: 'c2', name: 'Frontend', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };
  const TRACK = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null },
    id: 'w1', area_id: 'c1', title: 'Responsive mobile UI', sort: 1, cwd: '/tmp',
    pinned_at: 30, closed_at: null, created_at: 1, updated_at: 2,
  };
  const OTHER_TRACK = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null },
    id: 'w2', area_id: 'c1', title: 'Remote access', sort: 2, cwd: '/tmp',
    pinned_at: null, closed_at: null, created_at: 1, updated_at: 2,
  };

  /* The document the phone is meant to read. Prose carries the headings the
     Outline is built from; the task block is what the TASKS panel lists. */
  const REPORT_CARD = {
    id: 'card-report', track_id: 'w1', kind: 'track-report', title: 'Report', sort: 0, deletable: false,
    created_at: 1, updated_at: 2,
    payload: {
      schemaVersion: 3, docRev: 1,
      summary: 'Report stays at the root on a phone.',
      body: 'Mobile workspace direction',
      blocks: [
        {
          id: 'b-intro', kind: 'prose', rev: 1,
          payload: {
            markdown: '## Mobile workspace direction\n\nReport stays at the root. Navigation, cards and conversations arrive as focused pages instead of squeezing the document.\n',
          },
        },
        {
          id: 'b-task-layout', kind: 'task', rev: 1,
          payload: { key: 'mobile-layout', kind: 'codex', declared_by: 'spec', ready: true, goal: 'Validate the right-push interaction on a 390 × 844 viewport.' },
        },
        {
          id: 'b-why', kind: 'prose', rev: 1,
          payload: {
            markdown: '## Why this shape\n\nThe phone gets one clear reading surface. Secondary work remains one gesture away and always has an explicit route back to Report.\n',
          },
        },
        {
          id: 'b-task-touch', kind: 'task', rev: 1,
          payload: { key: 'touch-targets', kind: 'codex', declared_by: 'spec', ready: false, goal: 'Every control clears 44px.' },
        },
      ],
    },
  };
  const TERMINAL_CARD = {
    id: 'card-terminal', track_id: 'w1', kind: 'terminal', title: 'Implementation terminal', sort: 1,
    payload: { terminal_presentation: { source: 'manual', in_workbench: true }, }, deletable: true, created_at: 1, updated_at: 2,
  };
  const REVIEW_CARD = {
    id: 'card-review', track_id: 'w1', kind: 'codex', title: 'Design review', sort: 2,
    payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2,
  };

  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

  const transport: ApiTransportPort = {
    async send(request) {
      onRequest(request);
      const receipt = await receipts?.reply(request);
      if (receipt !== undefined) return receipt;
      if (request.path.endsWith('/planner/run')) return Promise.resolve(ok({ card_id: 'card-review', worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, manual_compact_supported: true, recovery: null, compact_outcome: null, context_usage_invalidated: false, running_turn: null }));
      if (request.path === '/api/settings') return Promise.resolve(ok({ settings: {} }));
      if (request.path === '/api/areas') return Promise.resolve(ok([{ ...AREA, name: areaName }, OTHER_AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK, OTHER_TRACK]));
      if (request.path === '/api/areas/c2/tracks') return Promise.resolve(ok([]));
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({
          track: TRACK, can_reopen: false, can_admin: false, can_close: true,
          cards: [REPORT_CARD, TERMINAL_CARD, REVIEW_CARD], overlays: [],
        }));
      }
      if (request.path === '/api/tracks/w1/report') return Promise.resolve(ok({ taskDiagnostics: [] }));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument,
    transport,
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }),
    client,
    cards: bootTestCardRuntime(),
    onSignOut: vi.fn(),
  });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <DepthFeedback />
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return router;
}
