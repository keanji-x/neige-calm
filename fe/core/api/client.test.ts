import { describe, expect, it, vi } from 'vitest';
import { z } from 'zod';

import { performApiRequest } from './client.js';

describe('core/api client behavior', () => {
  it('classifies 401, notifies once, and never retries', async () => {
    const send = vi.fn(() => Promise.resolve({
      status: 401,
      statusText: 'Unauthorized',
      body: { code: 'session_expired', error: 'Sign in again' },
    }));
    const notify = vi.fn();
    const result = await performApiRequest(
      { send },
      { method: 'GET', path: '/api/auth/whoami', responseSchema: z.unknown() },
      { subscribe: vi.fn(), notify },
    );

    expect(send).toHaveBeenCalledTimes(1);
    expect(notify).toHaveBeenCalledTimes(1);
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

  it('classifies 401 without an injected unauthorized channel', async () => {
    const result = await performApiRequest(
      { send: () => Promise.resolve({ status: 401, statusText: 'Unauthorized', body: undefined }) },
      { method: 'GET', path: '/api/auth/whoami', responseSchema: z.unknown() },
    );

    expect(result).toMatchObject({
      status: 'failed',
      error: { kind: 'unauthorized', status: 401 },
    });
  });

  it('keeps non-401 HTTP and thrown transport failures out of unauthorized', async () => {
    const notify = vi.fn();
    const channel = { subscribe: vi.fn(), notify };
    const http = await performApiRequest(
      { send: () => Promise.resolve({ status: 503, statusText: 'Unavailable', body: undefined }) },
      { method: 'GET', path: '/api/auth/whoami', responseSchema: z.unknown() },
      channel,
    );
    const cause = new Error('offline');
    const transport = await performApiRequest(
      { send: () => Promise.reject(cause) },
      { method: 'GET', path: '/api/auth/whoami', responseSchema: z.unknown() },
      channel,
    );

    expect(notify).not.toHaveBeenCalled();
    expect(http).toEqual({
      status: 'failed',
      error: { kind: 'http', status: 503, code: 'http_error', message: 'Unavailable', body: undefined },
    });
    expect(transport).toEqual({
      status: 'failed',
      error: { kind: 'transport', message: 'Transport request failed', cause },
    });
  });

  it('identifies a transport timeout for actionable user feedback', async () => {
    const cause = Object.assign(new Error('Request timed out.'), { name: 'TimeoutError' });
    const result = await performApiRequest(
      { send: () => Promise.reject(cause) },
      { method: 'DELETE', path: '/api/waves/w1', responseSchema: z.void() },
    );
    expect(result).toEqual({
      status: 'failed',
      error: { kind: 'transport', message: 'Request timed out.', cause },
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

  it('decodes a 204 as an explicit void response even if transport supplies a body', async () => {
    const result = await performApiRequest(
      { send: () => Promise.resolve({ status: 204, statusText: 'No Content', body: { stale: true } }) },
      { method: 'DELETE', path: '/api/coves/cove-1', responseSchema: z.void() },
    );

    expect(result).toEqual({ status: 'ready', value: undefined });
  });
});
