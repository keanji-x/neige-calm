// Test-only: the production `ReadReceiptsProvider` over a transport fake of `/api/read-receipts`. The fake
// stores what is posted as posted (a read replaces the instant); the kernel's own rule is pinned by its route tests.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ReadReceipt, ReadReceiptKind } from '../../../../core/domain/read-receipts.ts';
import { ReadReceiptsProvider } from './read-receipts.tsx';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

/**
 * Answers the receipt routes, or `undefined` for any other request. The default baseline is the far future:
 * everything has been read. `writeDelayMs` holds each write's answer.
 */
export function createReadReceiptsKernel({ baselineMs = Number.MAX_SAFE_INTEGER, receipts = [], fail = false, writeDelayMs = 0 }: Readonly<{
  baselineMs?: number; receipts?: readonly ReadReceipt[]; fail?: boolean; writeDelayMs?: number;
}> = {}) {
  const stored = new Map<string, ReadReceipt>(receipts.map((receipt) => [`${receipt.kind}:${receipt.targetId}`, receipt]));
  const requests: ApiRequest[] = [];
  const reply = async (request: ApiRequest): Promise<ApiTransportResponse | undefined> => {
    if (!request.path.startsWith('/api/read-receipts')) return undefined;
    requests.push(request);
    if (request.method === 'GET') return ok({ baselineMs, receipts: [...stored.values()] });
    if (writeDelayMs > 0) await new Promise((resolve) => setTimeout(resolve, writeDelayMs));
    if (fail) return { status: 500, statusText: 'Error', body: { error: 'unavailable', code: 'internal' } };
    const [, , , kind, encoded, verb] = request.path.split('/');
    const targetId = decodeURIComponent(encoded);
    const key = `${kind}:${targetId}`;
    const receipt: ReadReceipt = verb === 'read'
      ? { kind: kind as ReadReceiptKind, targetId, readThroughMs: (request.body as { readThroughMs: number }).readThroughMs, manualUnread: false }
      : { kind: kind as ReadReceiptKind, targetId, readThroughMs: stored.get(key)?.readThroughMs ?? 0, manualUnread: true };
    stored.set(key, receipt);
    return ok(receipt);
  };
  const transport: ApiTransportPort = { send: async (request) => {
    await Promise.resolve();
    return await reply(request) ?? { status: 404, statusText: 'Not Found', body: { error: 'not found', code: 'not_found' } };
  } };
  return {
    reply, transport, requests,
    posts: () => requests.filter((request) => request.method === 'POST'),
    stored: (kind: ReadReceiptKind, targetId: string) => stored.get(`${kind}:${targetId}`),
  };
}

export type ReadReceiptsKernel = ReturnType<typeof createReadReceiptsKernel>;

/** Wraps a tree in its own query client and the receipts provider over `kernel`. */
export function readReceiptsFixture(kernel: ReadReceiptsKernel = createReadReceiptsKernel()) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  return {
    kernel, client,
    wrap: (node: ReactNode) => <QueryClientProvider client={client}>
      <ReadReceiptsProvider transport={kernel.transport} unauthorized={unauthorized}>{node}</ReadReceiptsProvider>
    </QueryClientProvider>,
  };
}
