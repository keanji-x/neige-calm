// #1050 — production wiring contract for the cove task summary.
//
// This renders the real TanStack route/container and leaves the REST client,
// query option, hook, CovePage, and WaveRow unmocked. The only seam is fetch,
// so either dropping CoveComponent's taskSummary prop or changing the client
// URL makes the visible summary assertion fail.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { queryClient } from '../app/providers';
import { router } from '../app/router';
import { SessionContext } from '../app/SessionProvider';
import { ThemeProvider } from '../app/theme';
import type { KernelCoveTaskSummary } from '../api/wire';

const COVE_ID = 'cove route';
const SUMMARY_PATH = '/api/coves/cove%20route/task-summary';

const session = {
  userId: 'u-test',
  displayName: 'Test User',
  role: 'owner',
  sessionId: 's-test',
};

const counts = {
  pending: 8,
  inFlight: 10,
  done: 1,
  failed: 3,
  canceled: 4,
  legacyLive: 2,
  blockLive: 5,
  specLive: 7,
  userLive: 11,
};

function response(body: unknown): Response {
  return {
    ok: true,
    status: 200,
    statusText: 'OK',
    json: async () => body,
  } as Response;
}

function summaryResponse(truncated: boolean): KernelCoveTaskSummary {
  return {
    ...counts,
    truncated,
    waves: [{
      ...counts,
      waveId: 'w-production',
      title: 'Production route wave',
      lifecycle: 'draft',
      parentWaveId: null,
      specTaskCeiling: 32,
      treeTaskBudget: null,
    }],
  };
}

function installFetchMock(truncated: boolean): string[] {
  const requested: string[] = [];
  const taskSummary: KernelCoveTaskSummary = summaryResponse(truncated);
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const path = String(input);
    requested.push(path);
    if (path === '/api/coves') {
      return response([{
        id: COVE_ID,
        name: 'Production Cove',
        color: '#5a9',
        sort: 0,
        kind: 'user',
        created_at: 1,
        updated_at: 2,
      }]);
    }
    if (path === '/api/coves/cove%20route/waves') {
      return response([{
        id: 'w-production',
        cove_id: COVE_ID,
        title: 'Production route wave',
        sort: 0,
        lifecycle: 'draft',
        created_at: 1,
        updated_at: 2,
        terminal_at: null,
        pinned_at: null,
      }]);
    }
    if (path === SUMMARY_PATH) return response(taskSummary);
    if (path === '/api/overlays?entity_kind=wave') return response([]);
    throw new Error(`unmocked fetch: ${path}`);
  });
  vi.stubGlobal('fetch', fetchMock);
  return requested;
}

function renderProductionRoute() {
  render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <SessionContext.Provider value={session}>
          <RouterProvider router={router} />
        </SessionContext.Provider>
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

describe('cove task summary production route', () => {
  beforeEach(() => {
    queryClient.clear();
    window.history.replaceState({}, '', '/calm/cove/cove%20route');
  });

  afterEach(() => {
    cleanup();
    queryClient.clear();
    vi.unstubAllGlobals();
    window.history.replaceState({}, '', '/calm/');
  });

  it('fetches the encoded production URL and renders its summary through CoveComponent', async () => {
    const requested = installFetchMock(false);
    renderProductionRoute();

    await waitFor(() => {
      expect(screen.getByText('其中已投影 5')).toBeInTheDocument();
      expect(screen.getByText('存量未物化 2')).toBeInTheDocument();
    });
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
    expect(requested.filter((path) => path === SUMMARY_PATH)).toHaveLength(1);
  });

  it('renders the truncation warning from a true response through the production route', async () => {
    const requested = installFetchMock(true);
    renderProductionRoute();

    expect(await screen.findByRole('status')).toHaveTextContent(
      '任务统计仅显示存量未物化最多的前 200 个 wave；汇总仍包含全部 wave。',
    );
    expect(requested.filter((path) => path === SUMMARY_PATH)).toHaveLength(1);
  });
});
