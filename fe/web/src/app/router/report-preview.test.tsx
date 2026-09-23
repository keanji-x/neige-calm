// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, renderHook, waitFor } from '@testing-library/react';
import { createElement, type ReactNode } from 'react';
import { afterEach, describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ReportBlock } from '../../../../core/domain/report.ts';
import { ApiError, trackPreviewsQueryOptions, TRACK_PREVIEWS_POLL_MS } from '../providers/queries.ts';
import { previewResolutionOf, useReportPreviewResolver } from './report-preview.ts';

afterEach(cleanup);

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const fe = { key: 'fe', title: 'FE', port: 4050, live: true };

function recordingTransport(paths: string[]): ApiTransportPort {
  return {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      paths.push(request.path);
      return Promise.resolve({ status: 200, statusText: 'OK', body: { previews: [fe] } });
    },
  };
}

function wrapper(client: QueryClient) {
  return ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client }, children);
}

describe('previewResolutionOf', () => {
  it('answers by key from the data, then the error, then loading', () => {
    expect(previewResolutionOf({ data: { previews: [fe] }, isError: false, error: null }, 'fe'))
      .toEqual({ status: 'registered', preview: fe });
    expect(previewResolutionOf({ data: { previews: [fe] }, isError: false, error: null }, 'api'))
      .toEqual({ status: 'missing' });
    // A failed poll after a good read keeps the last answer.
    expect(previewResolutionOf({ data: { previews: [fe] }, isError: true, error: new Error('later') }, 'fe'))
      .toEqual({ status: 'registered', preview: fe });
    expect(previewResolutionOf({
      data: undefined, isError: true,
      error: new ApiError({ kind: 'http', status: 500, code: 'internal', message: 'boom' }),
    }, 'fe')).toEqual({ status: 'error', message: 'boom' });
    expect(previewResolutionOf({ data: undefined, isError: false, error: null }, 'fe')).toEqual({ status: 'loading' });
  });
});

describe('useReportPreviewResolver', () => {
  it('reads the track\'s previews once for every preview block and answers each by key', async () => {
    const paths: string[] = [];
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const blocks: ReportBlock[] = [
      { id: 'p-1', kind: 'prose', payload: { markdown: 'words' } },
      { id: 'b-1', kind: 'preview', payload: { key: 'fe' } },
      { id: 'b-2', kind: 'preview', payload: { key: 'api' } },
    ];
    const { result } = renderHook(
      () => useReportPreviewResolver(recordingTransport(paths), 'w/1', blocks, unauthorized),
      { wrapper: wrapper(client) },
    );
    expect(result.current('fe')).toEqual({ status: 'loading' });
    await waitFor(() => { expect(result.current('fe')).toEqual({ status: 'registered', preview: fe }); });
    expect(result.current('api')).toEqual({ status: 'missing' });
    expect(paths).toEqual(['/api/tracks/w%2F1/previews']);
    expect(client.getQueryData(['track-previews', 'w/1'])).toEqual({ previews: [fe] });
  });

  it('does not read at all when the report has no preview block', async () => {
    const paths: string[] = [];
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const blocks: ReportBlock[] = [{ id: 'p-1', kind: 'prose', payload: { markdown: 'words' } }];
    const { result } = renderHook(
      () => useReportPreviewResolver(recordingTransport(paths), 'w1', blocks, unauthorized),
      { wrapper: wrapper(client) },
    );
    await new Promise((resolve) => { setTimeout(resolve, 20); });
    expect(paths).toEqual([]);
    expect(result.current('fe')).toEqual({ status: 'loading' });
  });

  it('polls every five seconds: nothing announces a registration or a dev server coming up', () => {
    expect(TRACK_PREVIEWS_POLL_MS).toBe(5000);
    expect(trackPreviewsQueryOptions(recordingTransport([]), 'w1', unauthorized).refetchInterval)
      .toBe(TRACK_PREVIEWS_POLL_MS);
  });
});
