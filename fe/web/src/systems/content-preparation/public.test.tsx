import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { usePreparedContent, type PreparationStore } from './public.ts';

function controlled() {
  const cached = new Map<object, string>();
  const jobs: { input: object; signal: AbortSignal; resolve: (value: string) => void; reject: (reason: unknown) => void }[] = [];
  const prepare = vi.fn((input: object, signal: AbortSignal) => new Promise<string>((resolve, reject) => {
    jobs.push({ input, signal, resolve, reject });
  }));
  const store: PreparationStore<object, string> = { peek: input => cached.get(input), prepare };
  const complete = async (index: number, value: string) => {
    await act(async () => { jobs[index].resolve(value); await Promise.resolve(); });
  };
  const fail = async (index: number, cause: unknown) => {
    await act(async () => { jobs[index].reject(cause); await Promise.resolve(); });
  };
  return { store, cached, prepare, jobs, complete, fail };
}

afterEach(cleanup);

it('waits without execution while disabled and exposes completed data without mount permission', () => {
  const input = {}; const work = controlled();
  const view = renderHook(() => usePreparedContent('selection', input, work.store, false));
  expect(view.result.current).toMatchObject({ status: 'waiting', candidate: null, error: null });
  expect(work.prepare).not.toHaveBeenCalled();
  work.cached.set(input, 'completed');
  view.rerender();
  expect(view.result.current).toMatchObject({ status: 'ready', candidate: { input, value: 'completed' } });
  expect(work.prepare).not.toHaveBeenCalled();
});

it('retains a same-selection candidate until the current input completes', async () => {
  const first = {}; const next = {}; const work = controlled();
  const view = renderHook(({ input }) => usePreparedContent('selection', input, work.store, true), {
    initialProps: { input: first },
  });
  await work.complete(0, 'first result');
  const firstCandidate = view.result.current.candidate;
  view.rerender({ input: next });
  expect(view.result.current).toMatchObject({ status: 'preparing', candidate: firstCandidate });
  expect(work.jobs[1].input).toBe(next);
  await work.complete(1, 'next result');
  expect(view.result.current).toMatchObject({ status: 'ready', candidate: { input: next, value: 'next result' } });
});

it('rejects late A1 and B completion after A→B→A even when execution ignores abort', async () => {
  const a = {}; const b = {}; const work = controlled();
  const view = renderHook(({ key, input }) => usePreparedContent(key, input, work.store, true), {
    initialProps: { key: 'a', input: a },
  });
  view.rerender({ key: 'b', input: b });
  expect(view.result.current.candidate).toBeNull();
  view.rerender({ key: 'a', input: a });
  expect(work.jobs.map(job => job.signal.aborted)).toEqual([true, true, false]);
  await work.complete(0, 'obsolete A1'); await work.complete(1, 'obsolete B');
  expect(view.result.current).toMatchObject({ status: 'preparing', candidate: null, error: null });
  await work.complete(2, 'current A2');
  expect(view.result.current).toMatchObject({ status: 'ready', candidate: { input: a, value: 'current A2' } });
});

it('aborts disabled work, retains only candidate data and retries disabled without execution', async () => {
  const first = {}; const next = {}; const work = controlled();
  const view = renderHook(({ input, enabled }) => usePreparedContent('selection', input, work.store, enabled), {
    initialProps: { input: first, enabled: true },
  });
  await work.complete(0, 'retained');
  view.rerender({ input: next, enabled: true });
  view.rerender({ input: next, enabled: false });
  expect(work.jobs[1].signal.aborted).toBe(true);
  expect(view.result.current).toMatchObject({ status: 'waiting', candidate: { input: first, value: 'retained' }, error: null });
  act(() => { view.result.current.retry(); });
  expect(work.prepare).toHaveBeenCalledTimes(2);
  await work.complete(1, 'abandoned');
  expect(view.result.current).toMatchObject({ status: 'waiting', candidate: { input: first, value: 'retained' } });
  view.rerender({ input: next, enabled: true });
  expect(work.prepare).toHaveBeenCalledTimes(3);
  await work.complete(2, 'current');
  expect(view.result.current).toMatchObject({ status: 'ready', candidate: { input: next, value: 'current' } });
});

it('normalizes rejection, fences old retries and clears disabled error without executing', async () => {
  const first = {}; const next = {}; const work = controlled();
  const view = renderHook(({ input, enabled }) => usePreparedContent('selection', input, work.store, enabled), {
    initialProps: { input: first, enabled: true },
  });
  const obsoleteRetry = view.result.current.retry;
  await work.fail(0, 'Rejected preparation');
  expect(view.result.current).toMatchObject({ status: 'failed', candidate: null });
  expect(view.result.current.error?.message).toBe('Rejected preparation');
  act(() => { obsoleteRetry(); });
  expect(work.prepare).toHaveBeenCalledTimes(1);
  act(() => { view.result.current.retry(); });
  expect(work.prepare).toHaveBeenCalledTimes(2);
  view.rerender({ input: next, enabled: false });
  expect(view.result.current).toMatchObject({ status: 'waiting', candidate: null, error: null });
  act(() => { view.result.current.retry(); });
  expect(work.prepare).toHaveBeenCalledTimes(2);
  await work.fail(1, new Error('Canceled attempt'));
  expect(view.result.current).toMatchObject({ status: 'waiting', candidate: null, error: null });
});

it('normalizes a synchronous preparation result into the ready channel', async () => {
  const input = {};
  const prepare = vi.fn(() => 'synchronous value');
  const store: PreparationStore<object, string> = { peek: () => undefined, prepare };
  const view = renderHook(() => usePreparedContent('selection', input, store, true));
  await waitFor(() => expect(view.result.current.status).toBe('ready'));
  expect(view.result.current.candidate).toEqual({ input, value: 'synchronous value' });
  expect(prepare).toHaveBeenCalledTimes(1);
});

it('normalizes a synchronous throw and allows a later explicit retry', async () => {
  const input = {}; const error = new Error('Synchronous failure');
  const prepare = vi.fn<PreparationStore<object, string>['prepare']>()
    .mockImplementationOnce(() => { throw error; }).mockReturnValue('recovered');
  const store: PreparationStore<object, string> = { peek: () => undefined, prepare };
  const view = renderHook(() => usePreparedContent('selection', input, store, true));
  expect(view.result.current).toMatchObject({ status: 'failed', candidate: null, error });
  act(() => { view.result.current.retry(); });
  await waitFor(() => expect(view.result.current.status).toBe('ready'));
  expect(view.result.current.candidate?.value).toBe('recovered');
  expect(prepare).toHaveBeenCalledTimes(2);
});

it('owns StrictMode leases and keeps independent instances independent', async () => {
  const input = {}; const work = controlled();
  const first = renderHook(() => usePreparedContent('selection', input, work.store, true), { reactStrictMode: true });
  expect(work.jobs).toHaveLength(2);
  expect(work.jobs[0].signal.aborted).toBe(true);
  const second = renderHook(() => usePreparedContent('selection', input, work.store, true));
  first.unmount();
  expect(work.jobs[1].signal.aborted).toBe(true);
  expect(work.jobs[2].signal.aborted).toBe(false);
  await work.complete(0, 'strict replay'); await work.complete(1, 'unmounted');
  expect(second.result.current.candidate).toBeNull();
  await work.complete(2, 'independent');
  expect(second.result.current.candidate?.value).toBe('independent');
});

it('drops candidates immediately on store or selection scope changes', async () => {
  const input = {}; const first = controlled(); const second = controlled();
  const view = renderHook(({ key, store }) => usePreparedContent(key, input, store, true), {
    initialProps: { key: 'first', store: first.store },
  });
  await first.complete(0, 'first scope');
  view.rerender({ key: 'first', store: second.store });
  expect(view.result.current).toMatchObject({ status: 'preparing', candidate: null });
  await second.complete(0, 'second store');
  view.rerender({ key: 'second', store: second.store });
  expect(view.result.current).toMatchObject({ status: 'preparing', candidate: null });
});

it('uses Object.is identity for opaque inputs and cached values', () => {
  const prepare = vi.fn(() => Number.NaN);
  const store: PreparationStore<number, number> = { peek: () => Number.NaN, prepare };
  const view = renderHook(({ input }) => usePreparedContent('selection', input, store, false), {
    initialProps: { input: Number.NaN },
  });
  view.rerender({ input: Number.NaN });
  expect(view.result.current.status).toBe('ready');
  expect(view.result.current.candidate?.value).toBeNaN();
  expect(prepare).not.toHaveBeenCalled();
});
