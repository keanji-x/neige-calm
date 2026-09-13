// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, renderHook, waitFor } from '@testing-library/react';
import { createElement, type ReactNode } from 'react';
import { afterEach, describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ReportBlock } from '../../../../core/domain/report.ts';
import { ApiError } from '../providers/queries.ts';
import { seriesResolutionOf, useReportSeriesResolver } from './report-series.ts';

afterEach(cleanup);

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const SOURCE = 'neige://plugin/dev-neige-market/market.series';
const pending = { status: 'pending', view: 'line', field: 'close', period: 'day', range: '1Y' };

function series(id: string, rev: number): ReportBlock {
  return { id, kind: 'chart.series', rev, payload: { source: SOURCE, series: ['US:NVDA'] } };
}

describe('seriesResolutionOf', () => {
  it('reads data first, then the error, then loading', () => {
    expect(seriesResolutionOf({ data: { status: 'stale-rev', current_rev: 2 }, isError: false, error: null }))
      .toEqual({ status: 'stale-rev', current_rev: 2 });
    // A failed refetch keeps the last row on screen.
    expect(seriesResolutionOf({ data: { status: 'stale-rev', current_rev: 2 }, isError: true, error: new Error('later') }))
      .toEqual({ status: 'stale-rev', current_rev: 2 });
    expect(seriesResolutionOf({
      data: undefined, isError: true,
      error: new ApiError({ kind: 'http', status: 500, code: 'internal', message: 'boom' }),
    })).toEqual({ status: 'error', message: 'boom' });
    expect(seriesResolutionOf({ data: undefined, isError: true, error: new Error('plain') }))
      .toEqual({ status: 'error', message: 'Error: plain' });
    expect(seriesResolutionOf({ data: undefined, isError: false, error: null })).toEqual({ status: 'loading' });
  });
});

describe('useReportSeriesResolver', () => {
  it('runs one query per series block, keyed by (blockId, rev), and answers lookups by the same pair', async () => {
    const paths: string[] = [];
    const transport: ApiTransportPort = {
      send(request: ApiRequest): Promise<ApiTransportResponse> {
        paths.push(request.path);
        return Promise.resolve({ status: 200, statusText: 'OK', body: pending });
      },
    };
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const blocks: ReportBlock[] = [
      { id: 'p-1', kind: 'prose', payload: { markdown: 'words' } },
      series('b-1', 2),
      series('b-2', 7),
    ];
    const { result } = renderHook(() => useReportSeriesResolver(transport, 'w1', blocks, unauthorized), {
      wrapper: ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client }, children),
    });
    expect(result.current('b-1', 2)).toEqual({ status: 'loading' });
    await waitFor(() => { expect(result.current('b-2', 7)).toEqual(pending); });
    expect(result.current('b-1', 2)).toEqual(pending);
    // The prose block has no query; a rev the document did not render has none either.
    expect(result.current('p-1', 1)).toBeUndefined();
    expect(result.current('b-1', 3)).toBeUndefined();
    expect(paths.sort()).toEqual([
      '/api/tracks/w1/report/series/b-1?rev=2&detail=full',
      '/api/tracks/w1/report/series/b-2?rev=7&detail=full',
    ]);
    expect(client.getQueryData(['track-report-series', 'w1', 'b-1', 2])).toEqual(pending);
  });
});
