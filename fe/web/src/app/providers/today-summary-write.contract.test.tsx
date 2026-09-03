// @vitest-environment jsdom
//
// One claim, and it is a claim about what `useTodaySummaryMutation` does NOT do
// (#1253 §6).
//
// A 200 from `POST /api/today/summary` means the message was enqueued, not that
// the agent has written anything. The write lands later as a
// `wave.report_edited` event, which the bridge turns into `['today-launchpad']`
// and `['wave', id]`. Refetching either in `onSuccess` would fetch the OLD
// report — and worse, it would hide a broken invalidation chain behind a lucky
// refresh: the page would appear to update after a press even with both keys
// missing from the policy, which is the defect §6 exists to prevent.
//
// This file exists because the comment saying so was, at one point, wrong in
// each direction: it first claimed the document was not invalidated while the
// code invalidated it, and was then corrected to "the conversation lists — and
// only those" with nothing testing the hook at all. An assertion about what a
// function does not do needs a test more than most, not less.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { queryKeys, useTodaySummaryMutation } from './queries.ts';

afterEach(cleanup);
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

it('invalidates the conversation lists and nothing the document is read through', async () => {
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      expect(request.path).toBe('/api/today/summary');
      expect(request.method).toBe('POST');
      // No prompt: the message is synthesised server-side.
      expect(request.body).toBeUndefined();
      return Promise.resolve({
        status: 200, statusText: 'OK', body: { wave_id: 'lp', card_id: 'conv-1' },
      });
    },
  };

  const invalidated: readonly unknown[][] = [];
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const record = invalidated as unknown[][];
  const original = client.invalidateQueries.bind(client);
  client.invalidateQueries = (filters?: { queryKey?: readonly unknown[] }) => {
    if (filters?.queryKey !== undefined) record.push([...filters.queryKey]);
    return original(filters);
  };

  let write: (() => void) | null = null;
  function Probe() {
    write = useTodaySummaryMutation(transport, unauthorized).write;
    return null;
  }
  render(<QueryClientProvider client={client}><Probe /></QueryClientProvider>);
  await act(async () => { write?.(); await Promise.resolve(); });
  await act(async () => { await Promise.resolve(); });

  /*
   * The FULL set, not a denylist of the two keys that would hurt most.
   *
   * A first version asserted `toContain(conversations)` plus `not.toContain`
   * for `['today-launchpad']` and `['wave', id]`, which is what the comment in
   * `queries.ts` claims ("and only those") minus the "only": a third
   * invalidation added later would have passed. Equality is what makes the
   * sentence true.
   *
   * Why the two named absences matter enough to have been singled out: with
   * either of them here, `today-document.test.tsx`'s "redraws when the agent's
   * report edit arrives" cases could pass on this refetch instead of on the
   * invalidation policy they exist to pin — a green test measuring the wrong
   * mechanism.
   */
  expect(invalidated).toEqual([[...queryKeys.waveConversationsPrefix()]]);
});
