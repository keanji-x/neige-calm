import { QueryClient } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  plannerRunQueryOptions, trackBacklinksQueryOptions, trackConversationsQueryOptions,
  trackDetailQueryOptions, trackTaskVerdictsQueryOptions,
} from './queries.ts';
import { createFetchTransport } from './transport.ts';

afterEach(() => { vi.unstubAllGlobals(); });

describe('track query HTTP cancellation', () => {
  it.each(['detail', 'conversations', 'backlinks', 'tasks', 'planner'] as const)(
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
      const options = kind === 'detail' ? trackDetailQueryOptions(transport, 't1', unauthorized)
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
