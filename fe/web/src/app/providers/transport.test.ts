import { afterEach, describe, expect, it, vi } from 'vitest';

import { createFetchTransport } from './transport.ts';

const request = { method: 'DELETE', path: '/api/waves/w1', credentials: 'include' } as const;

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

describe('fetch transport cancellation', () => {
  it('relays caller abort to fetch', async () => {
    let fetchSignal: AbortSignal | undefined;
    vi.stubGlobal('fetch', vi.fn((_path: string, init?: RequestInit) => {
      fetchSignal = init?.signal as AbortSignal;
      return new Promise<Response>((_resolve, reject) => {
        fetchSignal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
      });
    }));
    const controller = new AbortController();
    const pending = createFetchTransport().send({ ...request, signal: controller.signal });
    controller.abort();
    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    expect(fetchSignal?.aborted).toBe(true);
  });

  it('aborts a request after the transport timeout', async () => {
    vi.useFakeTimers();
    let fetchSignal: AbortSignal | undefined;
    vi.stubGlobal('fetch', vi.fn((_path: string, init?: RequestInit) => {
      fetchSignal = init?.signal as AbortSignal;
      return new Promise<Response>((_resolve, reject) => {
        fetchSignal?.addEventListener('abort', () => reject(new DOMException('Request timed out.', 'TimeoutError')));
      });
    }));
    const pending = createFetchTransport().send(request);
    const rejected = expect(pending).rejects.toMatchObject({ name: 'TimeoutError' });
    await vi.advanceTimersByTimeAsync(30_000);
    await rejected;
    expect(fetchSignal?.aborted).toBe(true);
  });
});
