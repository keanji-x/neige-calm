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
import type { ReportBlock } from '../../cards/builtins/wave-report';
import type { WaveCardSlot } from '../../types';
import { WaveReportPage } from '../WaveReportPage';
import { ReportBlockView } from './index';

vi.mock('../../api/queries', () => ({
  useOverlaysByKindQuery: vi.fn(() => ({ data: [] })),
  useWaveBacklinksQuery: vi.fn(() => ({
    data: { backlinks: [], truncated: false, skipped_sources: 0 },
    error: null,
  })),
  useWaveFileContent: vi.fn(() => ({
    data: undefined,
    error: new TypeError('Failed to parse URL from /api/waves/wave_1/fs/report.md'),
    isLoading: false,
  })),
  useWaveFileList: vi.fn(() => ({
    data: [],
    error: null,
    isLoading: false,
  })),
  useWaveReportQuery: vi.fn(() => ({ data: undefined, refetch: vi.fn() })),
  useWavesByAreaQuery: vi.fn(() => ({ data: [] })),
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
                markdown: '[Target](neige://wave/wave_2#b_cafe)',
              },
            } as ReportBlock
          }
        />
      ),
    });
    const waveRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: '/wave/$waveId',
      component: () => null,
    });
    const router = createRouter({
      routeTree: rootRoute.addChildren([indexRoute, waveRoute]),
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
      '/calm/wave/wave_2#b_cafe',
    );
  });

  it('resolves links from the production flat-report fallback', async () => {
    const rootRoute = createRootRoute({ component: Outlet });
    const indexRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: '/',
      component: () => (
        <WaveReportPage
          wave={{
            id: 'wave_1',
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
                type: 'wave-report',
                id: 'report_1',
                summary: '',
                body: '[Flat target](neige://wave/wave_3#b_1f3a)',
              },
              deletable: false,
            } as WaveCardSlot,
          ]}
        />
      ),
    });
    const waveRoute = createRoute({
      getParentRoute: () => rootRoute,
      path: '/wave/$waveId',
      component: () => null,
    });
    const router = createRouter({
      routeTree: rootRoute.addChildren([indexRoute, waveRoute]),
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
    ).toHaveAttribute('href', '/calm/wave/wave_3#b_1f3a');
  });
});
