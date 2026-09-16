import { QueryClient, QueryClientProvider, onlineManager } from '@tanstack/react-query';
import { renderHook, cleanup } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { useTodayReportResetMutation, usePlannerMutations } from './queries.ts';

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
