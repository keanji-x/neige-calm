import { render, screen } from '@testing-library/react';
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from '@tanstack/react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';
import type { ReportBlock } from '../../cards/builtins/track-report';
import type { TrackCardSlot } from '../../types';
import { TrackReportPage } from '../TrackReportPage';
import { ReportBlockView } from './index';

vi.mock('../../api/queries', () => ({
  useOverlaysByKindQuery: vi.fn(() => ({ data: [] })),
  useTrackBacklinksQuery: vi.fn(() => ({
    data: { backlinks: [], truncated: false, skipped_sources: 0 },
    error: null,
  })),
  useTrackFileContent: vi.fn(() => ({
    data: undefined,
    error: new TypeError('Failed to parse URL from /api/tracks/track_1/fs/report.md'),
    isLoading: false,
  })),
  useTrackFileList: vi.fn(() => ({
    data: [],
    error: null,
    isLoading: false,
  })),
  useTrackReportQuery: vi.fn(() => ({ data: undefined, refetch: vi.fn() })),
  useTracksByAreaQuery: vi.fn(() => ({ data: [] })),
}));

vi.mock('../../cards/useCardOverlay', () => ({
  useCardOverlay: vi.fn(() => null),
}));

describe('report links with the real router', () => {
  it('resolves the route path, basepath, params, and hash from Link.to', async () => {
    const rootRoute = createRootRoute({ component: Outlet });
    const indexRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: '/',
      component: () => (
        <ReportBlockView
          block={
            {
              id: 'b_source',
              kind: 'prose',
              rev: 1,
              payload: {
                markdown: '[Target](neige://wave/track_2#b_cafe)',
              },
            } as ReportBlock
          }
        />
      ),
    });
    const trackRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: '/track/$trackId',
      component: () => null,
    });
    const router = createRouter({
      routeTree: rootRoute.addChildren([indexRoute, trackRoute]),
      history: createMemoryHistory({ initialEntries: ['/calm/'] }),
      basepath: '/calm',
    });

    await router.load();
    render(
      <QueryClientProvider client={new QueryClient()}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );

    expect(await screen.findByRole('link', { name: 'Target' })).toHaveAttribute(
      'href',
      '/calm/track/track_2#b_cafe',
    );
  });

  it('resolves links from the production flat-report fallback', async () => {
    const rootRoute = createRootRoute({ component: Outlet });
    const indexRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: '/',
      component: () => (
        <TrackReportPage
          track={{
            id: 'track_1',
            areaId: 'area_1',
            title: 'Flat report',
            lifecycle: 'draft',
            anyCardNeedsInput: false,
            progress: 0,
            eta: '',
            now: '',
            createdAt: 0,
            terminalAt: null,
            pinnedAt: null,
            cards: [],
          }}
          cards={[
            {
              kind: 'card',
              card: {
                type: 'track-report',
                id: 'report_1',
                summary: '',
                body: '[Flat target](neige://wave/track_3#b_1f3a)',
              },
              deletable: false,
            } as TrackCardSlot,
          ]}
        />
      ),
    });
    const trackRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: '/track/$trackId',
      component: () => null,
    });
    const router = createRouter({
      routeTree: rootRoute.addChildren([indexRoute, trackRoute]),
      history: createMemoryHistory({ initialEntries: ['/calm/'] }),
      basepath: '/calm',
    });

    await router.load();
    render(
      <QueryClientProvider client={new QueryClient()}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );

    expect(
      await screen.findByRole('link', { name: 'Flat target' }),
    ).toHaveAttribute('href', '/calm/track/track_3#b_1f3a');
  });
});
