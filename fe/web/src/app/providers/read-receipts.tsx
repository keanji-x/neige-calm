import { useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import { createContext, useContext, useEffect, useMemo, useRef, type ReactNode } from 'react';
import type { ApiOperation, ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  isUnread, markReadOperation, markUnreadOperation, readReceiptsOperation, receiptKey, toReadState, withMarkedRead,
  withMarkedUnread, withReceipt, type ReadReceipt, type ReadReceiptKind, type ReadState,
} from '../../../../core/domain/read-receipts.ts';
import { useState } from '../../ui/state/public.ts';
import { useConnectionStatus } from './connection-status.tsx';
import { queryKeys, runOperation } from './queries.ts';
import { useRecoveryMutation } from './recovery-mutation.ts';

/**
 * The kernel's read receipts (#2596), read through one query and written optimistically. Owner: the
 * root route, which mounts {@link ReadReceiptsProvider} with the app's transport; every reader below it
 * uses {@link useReadReceipts}. Writes go through the app's write admission (`useRecoveryMutation`), reach the
 * kernel in the order they were made for each target, and emit no event, so the query refetches on focus and
 * after the writes. Invariant: no read that started before a write settled is the final cached state.
 */
/** One query client's receipt writes: how many are in flight, and each target's last write in send order. */
type WriteLedger = { pending: number; tails: Map<string, Promise<void>> };
type ReceiptChannel = Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; ledgers: WeakMap<QueryClient, WriteLedger>;
}>;

const ReadReceiptsContext = createContext<ReceiptChannel | null>(null);

export function ReadReceiptsProvider({ transport, unauthorized, children }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; children: ReactNode;
}>) {
  const [ledgers] = useState(() => new WeakMap<QueryClient, WriteLedger>());
  const channel = useMemo(() => ({ transport, unauthorized, ledgers }), [transport, unauthorized, ledgers]);
  return <ReadReceiptsContext.Provider value={channel}>{children}</ReadReceiptsContext.Provider>;
}

export function readReceiptsQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.readReceipts(),
    queryFn: async (): Promise<ReadState> => toReadState(await runOperation(transport, readReceiptsOperation(), unauthorized)),
    staleTime: 0,
    refetchOnWindowFocus: true,
  };
}

export type ReadReceipts = Readonly<{
  /** Until the kernel's receipts are read nothing is unread: no verdict is not "everything is unread". */
  isUnread(kind: ReadReceiptKind, id: string, activityAt: number | null): boolean;
  /** Acknowledges `activityAt`. Always written: the kernel keeps the larger instant, and the cache may be behind it. */
  markRead(kind: ReadReceiptKind, id: string, activityAt: number | null): void;
  /** The reader's own write: rejects as any other write does (read it through `READ_RECEIPT_FAILURES`). */
  markUnread(kind: ReadReceiptKind, id: string): Promise<void>;
  /** The receipts have been read at least once. */
  ready: boolean;
}>;

type WriteIntent = Readonly<{
  kind: ReadReceiptKind; id: string; operation: ApiOperation<ReadReceipt>; optimistic: (state: ReadState) => ReadState;
}>;

async function write(client: QueryClient, channel: ReceiptChannel, admitted: ApiTransportPort, intent: WriteIntent): Promise<void> {
  const queryKey = queryKeys.readReceipts();
  const cancelReads = () => client.cancelQueries({ queryKey });
  let ledger = channel.ledgers.get(client);
  if (ledger === undefined) { ledger = { pending: 0, tails: new Map() }; channel.ledgers.set(client, ledger); }
  ledger.pending += 1;
  // This target's previous write reaches the kernel first.
  const key = receiptKey(intent.kind, intent.id);
  const previous = ledger.tails.get(key) ?? Promise.resolve();
  let settle!: () => void;
  const tail = new Promise<void>((resolve) => { settle = resolve; });
  ledger.tails.set(key, tail);
  try {
    // A read in flight may predate this write, the first read included: an invalidation would reuse it, so cancel.
    await cancelReads();
    client.setQueryData<ReadState>(queryKey, (state) => state === undefined ? state : intent.optimistic(state));
    await previous;
    const receipt = await runOperation(admitted, intent.operation, channel.unauthorized);
    // A later write of the same target keeps its own optimistic state until it answers.
    if (ledger.tails.get(key) === tail) {
      client.setQueryData<ReadState>(queryKey, (state) => state === undefined ? state : withReceipt(state, receipt));
    }
  } finally {
    settle();
    if (ledger.tails.get(key) === tail) ledger.tails.delete(key);
    ledger.pending -= 1;
    // A read started while this write was in flight may predate its commit. The last write to settle reads again.
    await cancelReads();
    if (ledger.pending === 0) void client.invalidateQueries({ queryKey });
  }
}

/**
 * Whether a write would be admitted now. In the bundled app that is the recovery owner's `connected` (the phase
 * its admission captures), as the gate publishes it; the web build admits every write.
 */
function useWriteAdmitted(): boolean {
  const connection = useConnectionStatus();
  return !__NC_BUNDLED__ || connection?.connected === true;
}

export function useReadReceipts(): ReadReceipts {
  const channel = useContext(ReadReceiptsContext);
  if (channel === null) throw new Error('useReadReceipts needs a ReadReceiptsProvider');
  const client = useQueryClient();
  const { data } = useQuery(readReceiptsQueryOptions(channel.transport, channel.unauthorized));
  const mutation = useRecoveryMutation(channel.transport, {
    mutationFn: (intent: WriteIntent, admitted: ApiTransportPort) => write(client, channel, admitted, intent),
  });
  // The writes stay the same functions while the data and the mutation move: the open acknowledgement follows neither.
  const send = useRef(mutation.mutateAsync);
  send.current = mutation.mutateAsync;
  const writes = useMemo(() => ({
    markRead(kind: ReadReceiptKind, id: string, activityAt: number | null) {
      const at = activityAt ?? 0;
      void send.current({ kind, id, operation: markReadOperation(kind, id, at),
        optimistic: (state) => withMarkedRead(state, kind, id, at) }).catch(() => undefined);
    },
    markUnread: (kind: ReadReceiptKind, id: string) => send.current({ kind, id, operation: markUnreadOperation(kind, id),
      optimistic: (state) => withMarkedUnread(state, kind, id) }),
  }), []);
  return useMemo<ReadReceipts>(() => ({
    ...writes,
    ready: data !== undefined,
    isUnread: (kind, id, activityAt) => data !== undefined && isUnread(data, kind, id, activityAt),
  }), [data, writes]);
}

/**
 * Acknowledges what the reader sees, as a standing condition: a visible, opened target is acknowledged whenever a
 * write can be admitted — on mount, when `activityAt` moves, on `openedAt` (a repeated selection), when the tab
 * becomes visible and when admission returns (a bundled resume) — whatever the cached receipts say. A background
 * tab never acknowledges work the reader has not seen, and nothing is attempted while no write would be admitted.
 */
export function useReadReceipt(kind: ReadReceiptKind, id: string | null, activityAt: number | null, enabled = true, openedAt?: number) {
  const { markRead } = useReadReceipts();
  const admitted = useWriteAdmitted();
  useEffect(() => {
    if (id === null || !enabled || !admitted) return;
    const markVisible = () => {
      if (document.visibilityState === 'visible') markRead(kind, id, activityAt);
    };
    markVisible();
    document.addEventListener('visibilitychange', markVisible);
    return () => document.removeEventListener('visibilitychange', markVisible);
    // Not on the receipts' data: re-running on a manual unread of the open target would undo it at once.
  }, [markRead, admitted, kind, id, activityAt, enabled, openedAt]);
}
