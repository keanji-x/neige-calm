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
 * everything has been read. `writeDelayMs` holds each write's answer. `holdReads` makes reads answer what
 * was stored when they arrived, only once released; `holdWrites` keeps each write uncommitted until released.
 */
export function createReadReceiptsKernel({ baselineMs = Number.MAX_SAFE_INTEGER, receipts = [], fail: failing = false, writeDelayMs = 0 }: Readonly<{
  baselineMs?: number; receipts?: readonly ReadReceipt[]; fail?: boolean; writeDelayMs?: number;
}> = {}) {
  const stored = new Map<string, ReadReceipt>(receipts.map((receipt) => [`${receipt.kind}:${receipt.targetId}`, receipt]));
  const requests: ApiRequest[] = [];
  let fail = failing;
  let failRead = false;
  let readGate: Promise<void> | null = null;
  let holdingWrites = false;
  const writeGates: Array<() => void> = [];
  // A fake kernel answers at most 100 writes: a runaway writer fails its test by count, never by exhausting memory.
  let writeCap = 100;
  let holdingAnswers = false;
  const answerGates: Array<() => void> = [];
  const reply = async (request: ApiRequest): Promise<ApiTransportResponse | undefined> => {
    if (!request.path.startsWith('/api/read-receipts')) return undefined;
    requests.push(request);
    if (request.method === 'GET') {
      if (failRead) return { status: 500, statusText: 'Error', body: { error: 'unavailable', code: 'internal' } };
      const snapshot = { baselineMs, receipts: [...stored.values()] };
      if (readGate !== null) await readGate;
      return ok(snapshot);
    }
    // Past the cap a write never answers: a runaway writer stops at a countable number of requests.
    if (requests.filter((sent) => sent.method === 'POST').length > writeCap) await new Promise<never>(() => undefined);
    if (holdingWrites) await new Promise<void>((resolve) => { writeGates.push(resolve); });
    if (writeDelayMs > 0) await new Promise((resolve) => setTimeout(resolve, writeDelayMs));
    if (fail) return { status: 500, statusText: 'Error', body: { error: 'unavailable', code: 'internal' } };
    const [, , , kind, encoded, verb] = request.path.split('/');
    const targetId = decodeURIComponent(encoded);
    const key = `${kind}:${targetId}`;
    const receipt: ReadReceipt = verb === 'read'
      ? { kind: kind as ReadReceiptKind, targetId, readThroughMs: (request.body as { readThroughMs: number }).readThroughMs, manualUnread: false }
      : { kind: kind as ReadReceiptKind, targetId, readThroughMs: stored.get(key)?.readThroughMs ?? 0, manualUnread: true };
    stored.set(key, receipt);
    if (holdingAnswers) await new Promise<void>((resolve) => { answerGates.push(resolve); });
    return ok(receipt);
  };
  const transport: ApiTransportPort = { send: async (request) => {
    await Promise.resolve();
    return await reply(request) ?? { status: 404, statusText: 'Not Found', body: { error: 'not found', code: 'not_found' } };
  } };
  return {
    reply, transport, requests,
    /** Holds the answer of every write from now on, after it is stored; `release` answers the oldest held one. */
    holdAnswers() {
      holdingAnswers = true;
      return { held: () => answerGates.length, release: () => { answerGates.shift()?.(); } };
    },
    /** Answers at most `count` writes; any later write stays unanswered. */
    capWrites(count: number) { writeCap = count; },
    /** Another device's write: stored as given, outside this reader's transport. */
    store(receipt: ReadReceipt) { stored.set(`${receipt.kind}:${receipt.targetId}`, receipt); },
    /** From now on every write answers 500 (`true`) or is stored (`false`). */
    failWrites(value: boolean) { fail = value; },
    /** From now on every read answers 500 (`true`) or the stored receipts (`false`). */
    failReads(value: boolean) { failRead = value; },
    /** Holds every read from now on; the returned function releases them and stops holding. */
    holdReads(): () => void {
      let release!: () => void;
      readGate = new Promise<void>((resolve) => { release = resolve; });
      return () => { readGate = null; release(); };
    },
    /** Holds every write from now on; `release` commits the oldest held one, `releaseLatest` the newest (a reordering network). */
    holdWrites() {
      holdingWrites = true;
      return {
        held: () => writeGates.length,
        release: () => { writeGates.shift()?.(); },
        releaseLatest: () => { writeGates.pop()?.(); },
      };
    },
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

/** `transport` with the receipt routes answered by `kernel` first: a harness records only its own requests. */
export function withReadReceipts(transport: ApiTransportPort, kernel: ReadReceiptsKernel = createReadReceiptsKernel()): ApiTransportPort {
  return { ...transport, send: async (request) => await kernel.reply(request) ?? await transport.send(request) };
}
