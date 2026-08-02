import { describe, expect, expectTypeOf, it, vi } from 'vitest';
import { z } from 'zod';

import { performApiRequest } from './client.js';
import type { ApiRequest, ApiResult, ApiTransportPort } from './types.js';

describe('core/api client contract', () => {
  it('freezes the injected transport shape and explicit result channel', () => {
    expectTypeOf<ApiRequest['credentials']>().toEqualTypeOf<'include'>();
    expectTypeOf<ApiTransportPort['send']>().returns.resolves.toHaveProperty('status');
    expectTypeOf<ApiResult<number>['status']>().toEqualTypeOf<'ready' | 'failed'>();
    expectTypeOf<Extract<ApiResult<number>, { status: 'failed' }>['error']['kind']>()
      .toEqualTypeOf<'unauthorized' | 'http' | 'transport' | 'decode'>();

    const compileOnly = false as boolean;
    if (compileOnly) {
      const transport = null as unknown as ApiTransportPort;
      // @ts-expect-error -- credentials are frozen to cookie-bearing requests.
      void transport.send({ method: 'GET', path: '/api/example', credentials: 'omit' });
    }
  });

  it('forwards a body as a complete JSON transport request', async () => {
    const send = vi.fn(() => Promise.resolve({ status: 200, statusText: 'OK', body: { value: 7 } }));
    await performApiRequest(
      { send },
      {
        method: 'PATCH',
        path: '/api/example/7',
        body: { title: 'independently rewritten' },
        responseSchema: z.object({ value: z.number() }),
      },
    );

    expect(send).toHaveBeenCalledExactlyOnceWith({
      method: 'PATCH',
      path: '/api/example/7',
      credentials: 'include',
      headers: { 'content-type': 'application/json' },
      body: { title: 'independently rewritten' },
    });
  });

  it('omits both body and headers keys when an operation has no body', async () => {
    const send = vi.fn<ApiTransportPort['send']>();
    send.mockResolvedValue({ status: 200, statusText: 'OK', body: undefined });
    await performApiRequest(
      { send },
      { method: 'GET', path: '/api/example', responseSchema: z.void() },
    );

    expect(send).toHaveBeenCalledExactlyOnceWith({
      method: 'GET',
      path: '/api/example',
      credentials: 'include',
    });
    const request = send.mock.calls[0]?.[0];
    expect(request).not.toHaveProperty('headers');
    expect(request).not.toHaveProperty('body');
  });
});
