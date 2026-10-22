import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useTrackClosedControl } from './closed-control.ts';

afterEach(cleanup);

it('keeps the newer Close control lock when an earlier request fails after refetch', async () => {
  let rejectFirst!: (reason: unknown) => void;
  let resolveSecond!: () => void;
  const first = new Promise<void>((_resolve, reject) => { rejectFirst = reject; });
  const second = new Promise<void>(resolve => { resolveSecond = resolve; });
  const patch = vi.fn().mockReturnValueOnce(first).mockReturnValueOnce(second);
  const { result, rerender } = renderHook(({ closed }) => useTrackClosedControl({
    trackId: 'w1', canClose: !closed, canReopen: closed, patch,
  }), { initialProps: { closed: false } });
  let firstResult!: Promise<unknown>;
  await act(async () => { firstResult = result.current.setClosed('close').catch((reason: unknown) => reason); await Promise.resolve(); });
  rerender({ closed: true });
  expect(result.current.pending).toBe(false);
  let secondResult!: Promise<boolean>;
  await act(async () => { secondResult = result.current.setClosed('reopen'); await Promise.resolve(); });
  expect(result.current.pending).toBe(true);
  await act(async () => { rejectFirst(new Error('late failure')); await firstResult; });
  expect(await firstResult).toEqual(new Error('Closing the track is unconfirmed.'));
  expect(result.current.feedback.error).toBe('Closing the track is unconfirmed.');
  const pendingAfterFailure = result.current.pending;
  let repeated!: boolean;
  await act(async () => { repeated = await result.current.setClosed('reopen'); });
  expect(pendingAfterFailure).toBe(true);
  expect(repeated).toBe(false);
  expect(patch).toHaveBeenCalledTimes(2);
  await act(async () => { resolveSecond(); await secondResult; });
  expect(result.current.pending).toBe(true);
  rerender({ closed: false });
  expect(result.current.pending).toBe(false);
});
