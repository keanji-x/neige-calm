// @vitest-environment jsdom
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { useConversationStop } from './stop.ts';

afterEach(cleanup);
function pendingRequest() {
  let resolve!: (receipt: { stopped: boolean }) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<{ stopped: boolean }>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

it('holds a synchronous lease across repeated gestures and releases it after an unconfirmed receipt', async () => {
  const pending = pendingRequest();
  const requestStop = vi.fn(() => pending.promise);
  const { result } = renderHook(() => useConversationStop({ cardId: 'a', canStop: true,
    responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop }));
  act(() => { result.current.interrupt(); result.current.interrupt(); });
  await act(async () => { await Promise.resolve(); });
  expect(requestStop).toHaveBeenCalledTimes(1);
  expect(result.current.feedback).toEqual({ kind: 'requesting' });
  await act(async () => { pending.resolve({ stopped: false }); await pending.promise; });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
  expect(result.current.pending).toBe(false);
});

it.each(['resolve', 'reject'] as const)('ignores an old view %s and its finalizer after A → B → A', async (ending) => {
  const old = pendingRequest();
  const next = pendingRequest();
  const requestStop = vi.fn().mockImplementationOnce(() => old.promise).mockImplementationOnce(() => next.promise);
  const { result, rerender } = renderHook(({ cardId }) => useConversationStop({ cardId,
    canStop: true, responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop }), { initialProps: { cardId: 'a' } });
  act(() => { result.current.interrupt(); });
  await act(async () => { await Promise.resolve(); });
  rerender({ cardId: 'b' });
  expect(result.current.feedback).toBeNull();
  rerender({ cardId: 'a' });
  expect(result.current.feedback).toBeNull();
  act(() => { result.current.interrupt(); });
  await act(async () => { await Promise.resolve(); });
  await act(async () => {
    if (ending === 'resolve') old.resolve({ stopped: false }); else old.reject(new Error('old error'));
    await old.promise.catch(() => undefined);
  });
  expect(result.current.feedback).toEqual({ kind: 'requesting' });
  expect(result.current.pending).toBe(true);
  act(() => { result.current.interrupt(); });
  expect(requestStop).toHaveBeenCalledTimes(2);
  await act(async () => { next.resolve({ stopped: false }); await next.promise; });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
});

it('retired handles cannot dispatch on another view or a no-longer-running response', async () => {
  const requestStop = vi.fn(() => Promise.resolve({ stopped: true }));
  const { result, rerender } = renderHook(({ cardId, canStop }) => useConversationStop({ cardId,
    canStop, responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop }), { initialProps: { cardId: 'a', canStop: true } });
  const old = result.current.interrupt;
  rerender({ cardId: 'b', canStop: true });
  act(() => { old(); });
  const current = result.current.interrupt;
  rerender({ cardId: 'b', canStop: false });
  act(() => { current(); });
  await act(async () => { await Promise.resolve(); });
  expect(requestStop).not.toHaveBeenCalled();
});

it('new terminal activity retires local uncertainty without manufacturing an outcome', async () => {
  const requestStop = vi.fn(() => Promise.resolve({ stopped: false }));
  const { result, rerender } = renderHook(({ completedRowId }) => useConversationStop({ cardId: 'a',
    canStop: true, responseEnded: false, historyKnown: true, newestRowId: Math.max(100, completedRowId ?? 0), completedRowId, requestStop }), { initialProps: { completedRowId: null as number | null } });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
  rerender({ completedRowId: 101 });
  expect(result.current.feedback).toBeNull();
});

it('loading previously unknown history is not evidence that this stop finished', async () => {
  const requestStop = vi.fn(() => Promise.resolve({ stopped: false }));
  const { result, rerender } = renderHook(({ historyKnown, completedRowId }) => useConversationStop({ cardId: 'a',
    canStop: true, responseEnded: false, historyKnown, newestRowId: historyKnown ? 100 : 0, completedRowId, requestStop }), { initialProps: { historyKnown: false, completedRowId: null as number | null } });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  rerender({ historyKnown: true, completedRowId: 10 });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
  rerender({ historyKnown: true, completedRowId: 101 });
  expect(result.current.feedback).toBeNull();
});

it.each([
  ['a lost answer', new ApiError({ kind: 'transport', message: 'Transport request failed' }), { kind: 'unconfirmed' }],
  ['a 5xx', new ApiError({ kind: 'http', status: 503, code: 'service_unavailable', message: 'down' }), { kind: 'unconfirmed' }],
  ['an error carrying no failure', new Error('boom'), { kind: 'unconfirmed' }],
  ['a dormant harness', new ApiError({ kind: 'http', status: 409, code: 'planner_harness_dormant', message: 'no live session' }),
    { kind: 'failed', message: 'no live session' }],
])('reads %s through the interrupt table and releases the lease', async (_case, error, feedback) => {
  const requestStop = vi.fn().mockRejectedValueOnce(error).mockResolvedValueOnce({ stopped: true });
  const { result } = renderHook(() => useConversationStop({ cardId: 'a', canStop: true,
    responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop }));
  await act(async () => { result.current.interrupt(); await Promise.resolve(); await Promise.resolve(); });
  expect(result.current.feedback).toEqual(feedback);
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  expect(requestStop).toHaveBeenCalledTimes(2);
});

it('keeps a true receipt pending while runtime reconciliation is unresolved', async () => {
  const requestStop = vi.fn(() => Promise.resolve({ stopped: true }));
  const { result } = renderHook(() => useConversationStop({ cardId: 'a', canStop: true,
    responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop }));
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  expect(result.current.feedback).toEqual({ kind: 'stopping' });
  expect(result.current.pending).toBe(true);
  act(() => { result.current.interrupt(); });
  expect(requestStop).toHaveBeenCalledTimes(1);
});

it('terminal progress retires a hung request and protects the next response lease', async () => {
  const old = pendingRequest();
  const next = pendingRequest();
  const requestStop = vi.fn().mockImplementationOnce(() => old.promise).mockImplementationOnce(() => next.promise);
  const { result, rerender } = renderHook(({ completedRowId }) => useConversationStop({ cardId: 'a', canStop: true,
    responseEnded: false, historyKnown: true, newestRowId: Math.max(100, completedRowId ?? 0), completedRowId, requestStop }), { initialProps: { completedRowId: null as number | null } });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  rerender({ completedRowId: 101 });
  expect(result.current.feedback).toBeNull();
  expect(result.current.pending).toBe(false);
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  expect(requestStop).toHaveBeenCalledTimes(2);
  await act(async () => { old.resolve({ stopped: false }); await old.promise; });
  expect(result.current.feedback).toEqual({ kind: 'requesting' });
  expect(result.current.pending).toBe(true);
  await act(async () => { next.resolve({ stopped: false }); await next.promise; });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
});

it('an ended runtime retires an accepted request even without a terminal row', async () => {
  const requestStop = vi.fn(() => Promise.resolve({ stopped: true }));
  const { result, rerender } = renderHook(({ responseEnded }) => useConversationStop({ cardId: 'a',
    canStop: !responseEnded, responseEnded, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop }),
    { initialProps: { responseEnded: false } });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  expect(result.current.feedback).toEqual({ kind: 'stopping' });
  rerender({ responseEnded: true });
  expect(result.current.feedback).toBeNull();
  expect(result.current.pending).toBe(false);
});

it('a delayed receipt cannot erase the terminal baseline adopted during the request', async () => {
  const old = pendingRequest();
  const { result, rerender } = renderHook(({ historyKnown, completedRowId }) => useConversationStop({ cardId: 'a',
    canStop: true, responseEnded: false, historyKnown, newestRowId: historyKnown ? 100 : 0, completedRowId, requestStop: () => old.promise }),
    { initialProps: { historyKnown: false, completedRowId: null as number | null } });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  rerender({ historyKnown: true, completedRowId: 10 });
  await act(async () => { old.resolve({ stopped: false }); await old.promise; });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
  rerender({ historyKnown: true, completedRowId: 101 });
  expect(result.current.feedback).toBeNull();
});

it('runtime settlement frees a hung receipt before the next response starts', async () => {
  const old = pendingRequest();
  const next = pendingRequest();
  const requestStop = vi.fn().mockImplementationOnce(() => old.promise).mockImplementationOnce(() => next.promise);
  const { result, rerender } = renderHook(({ responseEnded }) => useConversationStop({ cardId: 'a',
    canStop: !responseEnded, responseEnded, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop }),
    { initialProps: { responseEnded: false } });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  rerender({ responseEnded: true });
  expect(result.current.pending).toBe(false);
  rerender({ responseEnded: false });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  expect(requestStop).toHaveBeenCalledTimes(2);
  await act(async () => { old.resolve({ stopped: true }); await old.promise; });
  expect(result.current.feedback).toEqual({ kind: 'requesting' });
  await act(async () => { next.resolve({ stopped: false }); await next.promise; });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
});
