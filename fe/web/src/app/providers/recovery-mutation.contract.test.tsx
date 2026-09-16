import { QueryClient, QueryClientProvider, MutationCache, onlineManager } from '@tanstack/react-query';
import { renderHook, cleanup, act, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { usePlannerAttachments } from '../../features/planner/attachments.tsx';
import { useTodayReportResetMutation, usePlannerMutations, usePluginMutations } from './queries.ts';

afterEach(() => { cleanup(); onlineManager.setOnline(true); vi.unstubAllGlobals(); });
it('offline production reset rejects before mutation admission and never resumes a paused mutation', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true); onlineManager.setOnline(false);
  const access = new RecoveryAccess(); access.change('offline'); const send = vi.fn();
  const transport = createRecoveryTransports({ send }, access).business;
  const client = new QueryClient();
  const { result } = renderHook(() => useTodayReportResetMutation(transport, createUnauthorizedChannel({ enqueue: task => task() })), {
    wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
  await expect(result.current.reset()).rejects.toThrow();
  expect(client.getMutationCache().getAll()).toHaveLength(0);
  access.change('connected'); onlineManager.setOnline(true); await client.resumePausedMutations();
  expect(send).not.toHaveBeenCalled();
});
it('queued production model selections retain their original permit across recovery', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected');
  let finish!: (reply: ApiTransportResponse) => void;
  const send = vi.fn(() => new Promise<ApiTransportResponse>(resolve => { finish = resolve; }));
  const transport = createRecoveryTransports({ send }, access).business; const client = new QueryClient();
  const { result } = renderHook(() => usePlannerMutations(transport, 'card-a', createUnauthorizedChannel({ enqueue: task => task() })), {
    wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
  const first = result.current.setModel({ model: 'first', reasoning_effort: null }).catch(error => error as unknown);
  const second = result.current.setModel({ model: 'second', reasoning_effort: null }).catch(error => error as unknown);
  expect(send).toHaveBeenCalledOnce(); access.invalidate('recovering'); access.change('connected');
  finish({ status: 200, statusText: 'OK', body: { card_id: 'card-a', model: 'first', reasoning_effort: null, effort_adjusted: false, unknown_model: false } });
  await first; await second; expect(send).toHaveBeenCalledOnce();
});


it.each(['enable', 'uninstall'] as const)('releases the %s lease on a retained plugin hook after generation loss without committing stale effects', async (action) => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected');
  let reject!: (error: Error) => void;
  const send = vi.fn(() => new Promise<ApiTransportResponse>((_resolve, fail) => { reject = fail; }));
  const transport = createRecoveryTransports({ send }, access).business;
  const client = new QueryClient(); const invalidate = vi.spyOn(client, 'invalidateQueries');
  const { result } = renderHook(() => usePluginMutations(transport, createUnauthorizedChannel({ enqueue: task => task() })), {
    wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
  act(() => { if (action === 'enable') result.current.setEnabled('plugin-a', true); else result.current.uninstall('plugin-a'); });
  await waitFor(() => expect(result.current.pendingIds.has('plugin-a')).toBe(true));
  act(() => { access.invalidate('recovering'); access.change('connected'); reject(new Error('old request disconnected')); });
  await waitFor(() => expect(client.getMutationCache().getAll()[0]?.state.status).toBe('error'));
  expect(result.current.pendingIds.has('plugin-a')).toBe(false);
  expect(result.current.errors.size).toBe(0); expect(result.current.effectBoundaryIds.size).toBe(0);
  expect(invalidate).not.toHaveBeenCalled();
});
it('does not allocate a plugin pending lease when generation changes before onMutate executes', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected'); const send = vi.fn();
  const transport = createRecoveryTransports({ send }, access).business; // A real Query cache hook defers onMutate; no copy of the mutation implementation.
  const client = new QueryClient({ mutationCache: new MutationCache({ onMutate: () => Promise.resolve() }) });
  const { result } = renderHook(() => usePluginMutations(transport, createUnauthorizedChannel({ enqueue: task => task() })), {
    wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
  act(() => { result.current.setEnabled('plugin-a', true); access.invalidate('recovering'); access.change('connected'); });
  await waitFor(() => expect(client.getMutationCache().getAll()[0]?.state.status).toBe('error'));
  expect(send).not.toHaveBeenCalled(); expect(result.current.pendingIds.size).toBe(0);
});

it('a stale onMutate cannot erase an existing plugin error', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected');
  const send = vi.fn(() => Promise.reject(new Error('existing refusal')));
  const transport = createRecoveryTransports({ send }, access).business; // A real Query cache hook defers onMutate; no copy of the mutation implementation.
  const client = new QueryClient({ mutationCache: new MutationCache({ onMutate: () => Promise.resolve() }) });
  const { result } = renderHook(() => usePluginMutations(transport, createUnauthorizedChannel({ enqueue: task => task() })), {
    wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
  act(() => result.current.setEnabled('plugin-a', true));
  await waitFor(() => expect(result.current.errors.has('plugin-a')).toBe(true));
  const previous = result.current.errors.get('plugin-a');
  act(() => { result.current.setEnabled('plugin-a', false); access.invalidate('recovering'); access.change('connected'); });
  await waitFor(() => expect(client.getMutationCache().getAll()[1]?.state.status).toBe('error'));
  expect(result.current.errors.get('plugin-a')).toBe(previous);
  expect(result.current.pendingIds.size).toBe(0); expect(send).toHaveBeenCalledOnce();
});
it('releasing an old plugin lease preserves a newer pending intent on the same row', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected');
  const pending: { resolve(reply: ApiTransportResponse): void; reject(error: Error): void }[] = [];
  const send = vi.fn(() => new Promise<ApiTransportResponse>((resolve, reject) => { pending.push({ resolve, reject }); }));
  const transport = createRecoveryTransports({ send }, access).business; const client = new QueryClient();
  const invalidate = vi.spyOn(client, 'invalidateQueries');
  const { result } = renderHook(() => usePluginMutations(transport, createUnauthorizedChannel({ enqueue: task => task() })), {
    wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
  act(() => result.current.setEnabled('plugin-a', true)); await waitFor(() => expect(send).toHaveBeenCalledOnce());
  act(() => { access.invalidate('recovering'); access.change('connected'); result.current.setEnabled('plugin-a', false); });
  await waitFor(() => expect(send).toHaveBeenCalledTimes(2));
  act(() => pending[0].reject(new Error('old failure')));
  await waitFor(() => expect(client.getMutationCache().getAll()[0]?.state.status).toBe('error'));
  expect(result.current.pendingIds.has('plugin-a')).toBe(true); expect(invalidate).not.toHaveBeenCalled();
  act(() => pending[1].resolve({ status: 200, statusText: 'OK', body: { id: 'plugin-a', enabled: false } }));
  await waitFor(() => expect(result.current.pendingIds.size).toBe(0));
  expect(result.current.errors.size).toBe(0); expect(result.current.effectBoundaryIds.has('plugin-a')).toBe(true);
  expect(invalidate).toHaveBeenCalledOnce();
});


it('does not upload an old picked file after its asynchronous read crosses a recovery generation', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected');
  const send = vi.fn(() => Promise.resolve({ status: 200, statusText: 'OK', body: {
    attachmentId: 'image-a', contentType: 'image/png', size: 1, url: '/image-a',
  } }));
  const transport = createRecoveryTransports({ send }, access).business; const client = new QueryClient();
  const channel = createUnauthorizedChannel({ enqueue: task => task() });
  const { result } = renderHook(() => {
    const mutations = usePlannerMutations(transport, 'card-a', channel);
    return usePlannerAttachments(mutations.uploadAttachment, 'card-a');
  }, { wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider> });
  let read!: (bytes: ArrayBuffer) => void;
  const file = new File([], 'picked.png', { type: 'image/png' });
  Object.defineProperty(file, 'arrayBuffer', { value: () => new Promise<ArrayBuffer>(resolve => { read = resolve; }) });
  let done!: Promise<void>; act(() => { done = result.current.attach(file); });
  act(() => { access.invalidate('recovering'); access.change('connected'); read(new Uint8Array([1]).buffer); });
  await act(() => done);
  expect(send).not.toHaveBeenCalled(); expect(result.current.items).toHaveLength(0);
});
