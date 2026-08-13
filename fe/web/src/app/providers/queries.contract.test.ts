// @vitest-environment jsdom
// Invariants owned by the shared query layer.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { createElement, type ReactNode } from 'react';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { ApiError, coveListQueryOptions, useWorkspace, wavesInCoveQueryOptions } from './queries.ts';

function recordingTransport(reply: (request: ApiRequest) => ApiTransportResponse) {
  const paths: string[] = [];
  const transport: ApiTransportPort = {
    send(request) {
      paths.push(request.path);
      return Promise.resolve(reply(request));
    },
  };
  return { transport, paths };
}

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

const systemCove = { id: 'sys', name: 'system', color: '#000', sort: 0, kind: 'system', created_at: 1, updated_at: 1 };
const userCove = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };

describe('E2E-INV-SHELL-003 the system cove never reaches the workspace surface', () => {
  it('filters a system cove out of the list the shell renders', async () => {
    const { transport } = recordingTransport(() => ok([systemCove, userCove]));
    const coves = await coveListQueryOptions(transport).queryFn();
    expect(coves.map((cove) => cove.id)).toEqual(['c1']);
  });

  it('yields zero cove rows for a fresh workspace that only has the system cove', async () => {
    const { transport } = recordingTransport(() => ok([systemCove]));
    expect(await coveListQueryOptions(transport).queryFn()).toEqual([]);
  });

  it('orders the surviving coves by sort so the rail is stable', async () => {
    const { transport } = recordingTransport(() => ok([
      { ...userCove, id: 'b', sort: 3 }, { ...userCove, id: 'a', sort: 1 },
    ]));
    expect((await coveListQueryOptions(transport).queryFn()).map((cove) => cove.id)).toEqual(['a', 'b']);
  });
});

describe('failure channel', () => {
  it('rejects with ApiError carrying the normalized failure so Query can surface it', async () => {
    const { transport } = recordingTransport(() => ({ status: 500, statusText: 'Server Error', body: { code: 'boom', error: 'kaboom' } }));
    await expect(coveListQueryOptions(transport).queryFn()).rejects.toBeInstanceOf(ApiError);
  });

  it('exports cove and overlay read failures instead of collapsing them into healthy empty arrays', async () => {
    const { transport } = recordingTransport((request) => request.path === '/api/coves'
      ? { status: 500, statusText: 'Server Error', body: { error: 'coves down' } }
      : { status: 500, statusText: 'Server Error', body: { error: 'overlays down' } });
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client }, children);
    const { result } = renderHook(() => useWorkspace(transport), { wrapper });
    await waitFor(() => expect(result.current.covesError).toBeInstanceOf(ApiError));
    expect(result.current.overlaysError).toBeInstanceOf(ApiError);
    expect(result.current.waves).toEqual([]);
  });

  it('rejects when the payload does not match the schema instead of rendering junk', async () => {
    const { transport } = recordingTransport(() => ok([{ id: 'c1' }]));
    await expect(coveListQueryOptions(transport).queryFn()).rejects.toBeInstanceOf(ApiError);
  });
});

describe('wave list', () => {
  it('reads one cove at a time so each cove keeps its own cache entry', async () => {
    const { transport, paths } = recordingTransport(() => ok([]));
    await wavesInCoveQueryOptions(transport, 'c1').queryFn();
    await wavesInCoveQueryOptions(transport, 'c2').queryFn();
    expect(paths).toEqual(['/api/coves/c1/waves', '/api/coves/c2/waves']);
  });
});
