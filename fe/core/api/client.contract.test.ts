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

  it('sends cookie credentials and validates a successful response', async () => {
    const send = vi.fn(() => Promise.resolve({ status: 200, statusText: 'OK', body: { value: 7 } }));
    const result = await performApiRequest(
      { send },
      { method: 'GET', path: '/api/example', responseSchema: z.object({ value: z.number() }) },
    );

    expect(send).toHaveBeenCalledExactlyOnceWith({
      method: 'GET',
      path: '/api/example',
      credentials: 'include',
    });
    expect(result).toEqual({ status: 'ready', value: { value: 7 } });
  });

  it('classifies 401 without retrying inside core', async () => {
    const send = vi.fn(() => Promise.resolve({
      status: 401,
      statusText: 'Unauthorized',
      body: { code: 'session_expired', error: 'Sign in again' },
    }));
    const result = await performApiRequest(
      { send },
      { method: 'GET', path: '/api/auth/whoami', responseSchema: z.unknown() },
    );

    expect(send).toHaveBeenCalledTimes(1);
    expect(result).toEqual({
      status: 'failed',
      error: {
        kind: 'unauthorized',
        status: 401,
        code: 'session_expired',
        message: 'Sign in again',
        body: { code: 'session_expired', error: 'Sign in again' },
      },
    });
  });

  it('keeps non-401 HTTP and thrown transport failures out of the unauthorized branch', async () => {
    const http = await performApiRequest(
      { send: () => Promise.resolve({ status: 503, statusText: 'Unavailable', body: undefined }) },
      { method: 'GET', path: '/api/auth/whoami', responseSchema: z.unknown() },
    );
    const cause = new Error('offline');
    const transport = await performApiRequest(
      { send: () => Promise.reject(cause) },
      { method: 'GET', path: '/api/auth/whoami', responseSchema: z.unknown() },
    );

    expect(http).toEqual({
      status: 'failed',
      error: { kind: 'http', status: 503, code: 'http_error', message: 'Unavailable', body: undefined },
    });
    expect(transport).toEqual({
      status: 'failed',
      error: { kind: 'transport', message: 'Transport request failed', cause },
    });
  });

  it('reports response schema drift as decode data instead of throwing', async () => {
    const result = await performApiRequest(
      { send: () => Promise.resolve({ status: 200, statusText: 'OK', body: { value: '7' } }) },
      { method: 'GET', path: '/api/example', responseSchema: z.object({ value: z.number() }) },
    );

    expect(result.status).toBe('failed');
    if (result.status === 'failed') expect(result.error.kind).toBe('decode');
  });
});
