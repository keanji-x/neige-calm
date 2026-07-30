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
                markdown: '[Target](neige://wave/wave_2#b_target)',
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
      '/calm/wave/wave_2#b_target',
    );
  });
});
