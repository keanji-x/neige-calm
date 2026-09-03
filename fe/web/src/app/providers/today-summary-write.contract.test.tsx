// @vitest-environment jsdom
//
// One claim, and it is a claim about what `useTodaySummaryMutation` does NOT do
// (#1253 §6).
//
// A 200 from `POST /api/today/summary` means the message was enqueued, not that
// the agent has written anything. The write lands later as a
// `track.report_edited` event, which the bridge turns into `['today-launchpad']`
// and `['track', id]`. Refetching either in `onSuccess` would fetch the OLD
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

/**
 * Drive the hook once and report what it sent and what it invalidated.
 *
 * `hasLaunchpad` is the hook's own argument, not a fixture knob: it is what
 * `app/router` passes from the resolve, and it is the only thing that decides
 * whether the press prepares a launchpad first.
 */
async function press(
  { hasLaunchpad, ensure }: { hasLaunchpad: boolean; ensure?: ApiTransportResponse },
) {
  const paths: string[] = [];
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      paths.push(request.path);
      expect(request.method).toBe('POST');
      // No prompt: the message is synthesised server-side.
      expect(request.body).toBeUndefined();
      if (request.path === '/api/today/launchpad/ensure') {
        return Promise.resolve(ensure ?? { status: 201, statusText: 'Created', body: { track_id: 'lp' } });
      }
      expect(request.path).toBe('/api/today/summary');
      return Promise.resolve({
        status: 200, statusText: 'OK', body: { track_id: 'lp', card_id: 'conv-1' },
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
    write = useTodaySummaryMutation(transport, unauthorized, hasLaunchpad).write;
    return null;
  }
  render(<QueryClientProvider client={client}><Probe /></QueryClientProvider>);
  await act(async () => { write?.(); await Promise.resolve(); });
  await act(async () => { await Promise.resolve(); });
  await act(async () => { await Promise.resolve(); });
  return { paths, invalidated };
}

it('invalidates the conversation lists and nothing the document is read through', async () => {
  const { paths, invalidated } = await press({ hasLaunchpad: true });
  expect(paths).toEqual(['/api/today/summary']);

  /*
   * The FULL set, not a denylist of the two keys that would hurt most.
   *
   * A first version asserted `toContain(conversations)` plus `not.toContain`
   * for `['today-launchpad']` and `['track', id]`, which is what the comment in
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
  expect(invalidated).toEqual([[...queryKeys.trackConversationsPrefix()]]);
});

/*
 * ── The press that has to create the launchpad first ──────────────────────
 *
 * On a workspace with no launchpad, `POST /api/today/summary` alone can never
 * make one: it refuses an empty day before its own `ensure` step, so a quiet
 * day leaves the page with no launchpad, no Conversations `+`, and no way to
 * get either. The press runs `ensure` first, and only then.
 *
 * The resolve is refetched here and only here — that is the one key this hook
 * adds to the set the case above pins, and the reason it is not the forbidden
 * refetch is that `ensure`'s answer really did change what the resolve says.
 */
it('prepares the launchpad before writing when there is none, and refetches the resolve', async () => {
  const { paths, invalidated } = await press({ hasLaunchpad: false });
  expect(paths).toEqual(['/api/today/launchpad/ensure', '/api/today/summary']);
  // `onSuccess` before `onSettled`, which is react-query's order and not
  // something this contract has an opinion about — the claim is the set.
  expect(invalidated).toEqual([
    [...queryKeys.trackConversationsPrefix()],
    [...queryKeys.todayLaunchpad()],
  ]);
});

/*
 * `ensure` answers 503 for "the launchpad is there, its harness would not
 * start" — so the failing press may still have created the thing the resolve
 * reports on. Refetching only on success would leave the page insisting there
 * is no launchpad about one that now exists, with the `+` withheld with it.
 */
it('refetches the resolve even when the preparation fails', async () => {
  const { paths, invalidated } = await press({
    hasLaunchpad: false,
    ensure: { status: 503, statusText: 'Service Unavailable', body: { error: 'harness down' } },
  });
  expect(paths).toEqual(['/api/today/launchpad/ensure']);
  expect(invalidated).toEqual([[...queryKeys.todayLaunchpad()]]);
});
