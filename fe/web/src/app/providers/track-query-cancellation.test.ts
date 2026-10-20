import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  areaListQueryOptions, tracksInAreaQueryOptions, plannerRunQueryOptions, trackBacklinksQueryOptions, trackConversationsQueryOptions,
  trackDetailQueryOptions, trackTaskVerdictsQueryOptions,
} from './queries.ts';
import { retireAreaQueries } from './area-query-lifecycle.ts';
import { createFetchTransport } from './transport.ts';

afterEach(() => { vi.unstubAllGlobals(); });

describe('track query HTTP cancellation', () => {
  it.each(['areas', 'tracks', 'detail', 'conversations', 'backlinks', 'tasks', 'planner'] as const)(
    'aborts the obsolete %s HTTP request before starting its replacement', async (kind) => {
      const signals: AbortSignal[] = [];
      vi.stubGlobal('fetch', (_path: string, init: RequestInit) => {
        const signal = init.signal as AbortSignal;
        signals.push(signal);
        return new Promise<Response>((_resolve, reject) => {
          signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
        });
      });
      const transport = createFetchTransport();
      const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
      const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
      const options = kind === 'areas' ? areaListQueryOptions(transport, unauthorized)
        : kind === 'tracks' ? tracksInAreaQueryOptions(transport, 'a1', unauthorized)
        : kind === 'detail' ? trackDetailQueryOptions(transport, 't1', unauthorized)
        : kind === 'conversations' ? trackConversationsQueryOptions(transport, 't1', unauthorized)
          : kind === 'backlinks' ? trackBacklinksQueryOptions(transport, 't1', unauthorized)
            : kind === 'tasks' ? trackTaskVerdictsQueryOptions(transport, 't1', unauthorized, null)
              : plannerRunQueryOptions(transport, 'c1', unauthorized);
      try {
        const first = client.fetchQuery<unknown>(options).catch(() => undefined);
        expect(signals).toHaveLength(1);
        await client.cancelQueries({ queryKey: options.queryKey });
        await first;
        expect(signals[0]?.aborted).toBe(true);
        const second = client.fetchQuery<unknown>(options).catch(() => undefined);
        expect(signals).toHaveLength(2);
        expect(signals[1]?.aborted).toBe(false);
        await client.cancelQueries({ queryKey: options.queryKey });
        await second;
        expect(signals[1]?.aborted).toBe(true);
      } finally {
        client.clear();
      }
    },
  );
});

// The workspace exposes an explicit Retry action; terminal authorization/missing reads never loop.
describe('workspace list terminal HTTP failures', () => {
  it.each([403, 404])('settles both list reads without retrying HTTP %s', async status => {
    const fetch = vi.fn(() => Promise.resolve(new Response(JSON.stringify({ error: 'unavailable' }), { status })));
    vi.stubGlobal('fetch', fetch);
    const transport = createFetchTransport();
    const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
    const client = new QueryClient({ defaultOptions: { queries: { retry: true } } });
    try {
      for (const options of [areaListQueryOptions(transport, unauthorized), tracksInAreaQueryOptions(transport, 'a1', unauthorized)]) {
        await expect(client.fetchQuery<unknown>(options)).rejects.toThrow();
        expect(client.getQueryState(options.queryKey)).toMatchObject({ status: 'error', fetchStatus: 'idle', fetchFailureCount: 1 });
      }
      expect(fetch).toHaveBeenCalledTimes(2);
    } finally { client.clear(); }
  });
});

// Exercise Query's retryer with production options and transport, including its browser budget.
describe('Area Track read automatic recovery', () => {
  afterEach(() => { vi.useRealTimers(); });

  it.each(['http', 'transport', 'exhausted', 'exhausted-transport', '401', '403', '404'] as const)(
    'settles %s through the production query options', async kind => {
      vi.useFakeTimers();
      const fetch = vi.fn(() => {
        if (kind === 'exhausted-transport' || (kind === 'transport' && fetch.mock.calls.length === 1)) return Promise.reject(new TypeError('offline'));
        const status = kind === 'http' ? (fetch.mock.calls.length === 1 ? 500 : 200)
          : kind === 'transport' ? 200 : kind === 'exhausted' ? 500 : Number(kind);
        return Promise.resolve(new Response(JSON.stringify(status === 200 ? [] : { error: 'unavailable' }), { status }));
      });
      vi.stubGlobal('fetch', fetch);
      const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
      const notified = vi.fn();
      unauthorized.subscribe(notified);
      const client = new QueryClient();
      const options = tracksInAreaQueryOptions(createFetchTransport(), 'a1', unauthorized);
      const observer = new QueryObserver(client, options);
      const unsubscribe = observer.subscribe(() => undefined);
      try {
        await vi.advanceTimersByTimeAsync(10_000);
        const recovered = kind === 'http' || kind === 'transport';
        expect(fetch).toHaveBeenCalledTimes(recovered ? 2 : kind.startsWith('exhausted') ? 4 : 1);
        expect(observer.getCurrentResult()).toMatchObject({
          status: recovered ? 'success' : 'error', fetchStatus: 'idle',
          failureCount: recovered ? 0 : kind.startsWith('exhausted') ? 4 : 1,
          ...(recovered ? { data: [] } : {}),
        });
        expect(notified).toHaveBeenCalledTimes(kind === '401' ? 1 : 0);
      } finally { unsubscribe(); client.clear(); }
    },
  );

  it.each(['cancel', 'retire'] as const)('does not request again after %s during backoff', async action => {
    vi.useFakeTimers();
    const fetch = vi.fn(() => Promise.resolve(new Response('{"error":"temporary"}', { status: 500 })));
    vi.stubGlobal('fetch', fetch);
    const client = new QueryClient();
    const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
    const options = tracksInAreaQueryOptions(createFetchTransport(), 'a1', unauthorized);
    const observer = new QueryObserver(client, options);
    const unsubscribe = observer.subscribe(() => undefined);
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(observer.getCurrentResult()).toMatchObject({ fetchStatus: 'fetching', failureCount: 1 });
      if (action === 'retire') retireAreaQueries(client, 'a1');
      else await client.cancelQueries({ queryKey: options.queryKey });
      unsubscribe();
      await vi.advanceTimersByTimeAsync(10_000);
      expect(fetch).toHaveBeenCalledTimes(1);
      if (action === 'retire') expect(client.getQueryState(options.queryKey)).toBeUndefined();
      else expect(client.getQueryState(options.queryKey)).toMatchObject({ fetchStatus: 'idle' });
    } finally { unsubscribe(); client.clear(); }
  });
});
