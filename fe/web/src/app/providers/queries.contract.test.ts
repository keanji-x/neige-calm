// @vitest-environment jsdom
// Invariants owned by the shared query layer.
import { describe, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { ApiError, coveListQueryOptions, harnessItemsQueryOptions, wavesInCoveQueryOptions } from './queries.ts';

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

describe('spec history pagination', () => {
  it('uses the first (oldest) row from the ascending first page as the second-page after_id', async () => {
    const firstPage = Array.from({ length: 300 }, (_, index) => ({
      id: 701 + index, runtime_id: 'runtime', card_id: 'card', wave_id: 'wave', thread_id: 'thread',
      turn_id: null, item_uuid: null, item_type: 'agent_message', method: 'item/completed',
      params: '{}', created_at_ms: index,
    }));
    const { transport, paths } = recordingTransport(() => ok(firstPage));
    const options = harnessItemsQueryOptions(transport, 'card');
    const page = await options.queryFn({ pageParam: 0 });
    const cursor = options.getNextPageParam(page);
    expect(cursor).toBe(701);
    await options.queryFn({ pageParam: cursor ?? 0 });
    expect(paths[1]).toContain('after_id=701');
  });
});
