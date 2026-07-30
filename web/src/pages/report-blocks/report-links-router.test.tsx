import { render, screen } from '@testing-library/react';
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from '@tanstack/react-router';
import { describe, expect, it } from 'vitest';
import type { ReportBlock } from '../../cards/builtins/wave-report';
import { ReportMarkdown } from '../WaveReportPage';
import { ReportBlockView } from './index';

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
    render(<RouterProvider router={router} />);

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
        <ReportMarkdown body="[Flat target](neige://wave/wave_3#b_1f3a)" />
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
    render(<RouterProvider router={router} />);

    expect(
      await screen.findByRole('link', { name: 'Flat target' }),
    ).toHaveAttribute('href', '/calm/wave/wave_3#b_1f3a');
  });
});
