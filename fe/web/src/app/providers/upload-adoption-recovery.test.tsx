import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { usePlannerAttachments } from '../../features/planner/attachments.tsx';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { usePlannerMutations } from './queries.ts';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it.each([false, true])('consumes the upload only under its admitted generation (interrupted=%s)', async interrupted => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected');
  const uploaded = { attachmentId: 'uploaded.png', contentType: 'image/png', size: 4,
    url: '/api/cards/card-a/planner/attachments/uploaded.png' };
  const send = vi.fn(() => Promise.resolve({ status: 200, statusText: 'OK', body: uploaded }));
  const transport = createRecoveryTransports({ send }, access).business;
  const client = new QueryClient();
  let delivered = false;
  const hook = renderHook(() => {
    const mutations = usePlannerMutations(transport, 'card-a', createUnauthorizedChannel({ enqueue: task => task() }));
    return usePlannerAttachments(async (readBytes, type) => {
      const result = await mutations.uploadAttachment(readBytes, type);
      delivered = true;
      // The provider has finished. Revoke permission before the feature resumes.
      if (interrupted) access.invalidate('paused');
      return result;
    }, 'card-a');
  }, { wrapper: ({ children }) => <QueryClientProvider client={client}>{children}</QueryClientProvider> });
  try {
    await act(() => hook.result.current.attach(new File([new Uint8Array([1, 2, 3, 4])], 'a.png', { type: 'image/png' })));
    expect(delivered).toBe(true); expect(send).toHaveBeenCalledOnce();
    expect(hook.result.current.busy).toBe(false);
    expect(hook.result.current.ids).toEqual(interrupted ? [] : ['uploaded.png']);
    if (interrupted) {
      expect(access.read().phase).toBe('paused');
      expect(hook.result.current.error).not.toBeNull();
      act(() => access.change('connected'));
      expect(hook.result.current.ids).toEqual([]); expect(send).toHaveBeenCalledOnce();
    }
  } finally { hook.unmount(); client.clear(); }
});
