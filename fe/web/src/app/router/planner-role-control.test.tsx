import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { usePlannerRoleControl } from './planner-role.ts';

afterEach(cleanup);

it('keeps the newer Planner role lock when an earlier request fails after refetch', async () => {
  let rejectFirst!: (reason: unknown) => void;
  let resolveSecond!: () => void;
  const first = new Promise<void>((_resolve, reject) => { rejectFirst = reject; });
  const second = new Promise<void>(resolve => { resolveSecond = resolve; });
  const setAdmin = vi.fn().mockReturnValueOnce(first).mockReturnValueOnce(second);
  const { result, rerender } = renderHook(({ admin }) => usePlannerRoleControl({
    trackId: 'w1', adminGrantedAt: admin ? 99 : null, canAdmin: true, setAdmin,
  }), { initialProps: { admin: false } });
  await act(async () => { result.current.onChange('admin'); await Promise.resolve(); });
  rerender({ admin: true });
  expect(result.current.isDisabled).toBe(false);
  await act(async () => { result.current.onChange('member'); await Promise.resolve(); });
  expect(result.current.isDisabled).toBe(true);
  await act(async () => { rejectFirst(new Error('late failure')); await first.catch(() => undefined); });
  expect(result.current.feedback.error).toBe('Making the track an Area admin is unconfirmed.');
  const disabledAfterFailure = result.current.isDisabled;
  await act(async () => { result.current.onChange('member'); await Promise.resolve(); });
  expect(disabledAfterFailure).toBe(true);
  expect(setAdmin).toHaveBeenCalledTimes(2);
  await act(async () => { resolveSecond(); await second; });
  expect(result.current.isDisabled).toBe(true);
  rerender({ admin: false });
  expect(result.current.isDisabled).toBe(false);
});
