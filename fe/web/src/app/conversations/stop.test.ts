// @vitest-environment jsdom
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useConversationStop } from './stop.ts';

afterEach(cleanup);
function pendingRequest() {
  let resolve!: (receipt: { stopped: boolean }) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<{ stopped: boolean }>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const failureText = (error: unknown) => error instanceof Error ? error.message : 'Request failed.';

it('holds a synchronous lease across repeated gestures and releases it after an unconfirmed receipt', async () => {
  const pending = pendingRequest();
  const requestStop = vi.fn(() => pending.promise);
  const { result } = renderHook(() => useConversationStop({ cardId: 'a', canStop: true,
    responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop, failureText }));
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
    canStop: true, responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop, failureText }), { initialProps: { cardId: 'a' } });
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
    canStop, responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop, failureText }), { initialProps: { cardId: 'a', canStop: true } });
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
    canStop: true, responseEnded: false, historyKnown: true, newestRowId: Math.max(100, completedRowId ?? 0), completedRowId, requestStop, failureText }), { initialProps: { completedRowId: null as number | null } });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
  rerender({ completedRowId: 101 });
  expect(result.current.feedback).toBeNull();
});

it('loading previously unknown history is not evidence that this stop finished', async () => {
  const requestStop = vi.fn(() => Promise.resolve({ stopped: false }));
  const { result, rerender } = renderHook(({ historyKnown, completedRowId }) => useConversationStop({ cardId: 'a',
    canStop: true, responseEnded: false, historyKnown, newestRowId: historyKnown ? 100 : 0, completedRowId, requestStop, failureText }), { initialProps: { historyKnown: false, completedRowId: null as number | null } });
  await act(async () => { result.current.interrupt(); await Promise.resolve(); });
  rerender({ historyKnown: true, completedRowId: 10 });
  expect(result.current.feedback).toEqual({ kind: 'unconfirmed' });
  rerender({ historyKnown: true, completedRowId: 101 });
  expect(result.current.feedback).toBeNull();
});

it('does not process an old error after its view unmounts', async () => {
  const request = pendingRequest();
  const failureText = vi.fn(() => 'Old error');
  const { result, unmount } = renderHook(() => useConversationStop({ cardId: 'a', canStop: true,
    responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop: () => request.promise, failureText }));
  act(() => { result.current.interrupt(); });
  await act(async () => { await Promise.resolve(); });
  unmount();
  await act(async () => { request.reject(new Error('late')); await request.promise.catch(() => undefined); });
  expect(failureText).not.toHaveBeenCalled();
});

it('keeps a true receipt pending while runtime reconciliation is unresolved', async () => {
  const requestStop = vi.fn(() => Promise.resolve({ stopped: true }));
  const { result } = renderHook(() => useConversationStop({ cardId: 'a', canStop: true,
    responseEnded: false, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop, failureText }));
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
    responseEnded: false, historyKnown: true, newestRowId: Math.max(100, completedRowId ?? 0), completedRowId, requestStop, failureText }), { initialProps: { completedRowId: null as number | null } });
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
    canStop: !responseEnded, responseEnded, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop, failureText }),
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
    canStop: true, responseEnded: false, historyKnown, newestRowId: historyKnown ? 100 : 0, completedRowId, requestStop: () => old.promise, failureText }),
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
    canStop: !responseEnded, responseEnded, historyKnown: true, newestRowId: 100, completedRowId: null, requestStop, failureText }),
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
