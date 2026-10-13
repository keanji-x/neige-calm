import { hashKey, onlineManager, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
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

/** The provider's writes: how many are in flight, each target's last write, and whether the next read is its own. */
type WriteLedger = { pending: number; tails: Map<string, Promise<void>>; reconciling: boolean };
type Target = Readonly<{ kind: ReadReceiptKind; id: string }>;
type Owed = Target & Readonly<{ at: number }>;
type WriteIntent = Target & Readonly<{ operation: ApiOperation<ReadReceipt>; optimistic: (state: ReadState) => ReadState }>;
type ReceiptChannel = Pick<ReadReceipts, 'markRead' | 'markUnread'> & Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel }>;

const ReadReceiptsContext = createContext<ReceiptChannel | null>(null);

async function write(
  client: QueryClient, ledger: WriteLedger, admitted: ApiTransportPort, unauthorized: UnauthorizedChannel, intent: WriteIntent,
): Promise<void> {
  const queryKey = queryKeys.readReceipts();
  const cancelReads = () => client.cancelQueries({ queryKey });
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
    const receipt = await runOperation(admitted, intent.operation, unauthorized);
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
    // Its own read-back is no flush trigger (a failing write would loop on it), for exactly that fetch: success or not.
    if (ledger.pending === 0) { ledger.reconciling = true; void client.invalidateQueries({ queryKey }).finally(() => { ledger.reconciling = false; }); }
  }
}

/**
 * The kernel's read receipts (#2596), owned by the root route. Every receipt write goes through the app's write
 * admission (`useRecoveryMutation`), in the order made per target; no read that started before a write settled is the
 * final cached state. `markRead` owes an acknowledgement (the larger instant per target) and `flush` sends what is
 * owed, forgetting it once stored. A refused or failed send stays owed until a trigger: a new record, `onlineManager`
 * going online, the published connection turning connected, or a receipts read this provider did not cause. Triggers
 * flush only what is owed, so none can clear a manual unread, and admission is never predicted here. An owed
 * acknowledgement outlives navigation, unmount and a hidden tab: it was seen.
 */
export function ReadReceiptsProvider({ transport, unauthorized, children }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; children: ReactNode;
}>) {
  const client = useQueryClient();
  const [{ ledger, owed, sending, missed }] = useState(() => ({ ledger: { pending: 0, tails: new Map<string, Promise<void>>(), reconciling: false } satisfies WriteLedger,
    owed: new Map<string, Owed>(), sending: new Map<string, Owed>(), missed: new Set<string>() }));
  const mutation = useRecoveryMutation(transport, { mutationFn: (intent: WriteIntent, admitted: ApiTransportPort) =>
    write(client, ledger, admitted, unauthorized, intent) });
  const send = useRef(mutation.mutateAsync);
  send.current = mutation.mutateAsync;
  // An owed acknowledgement is its record: a send settles only the record it sent, never a later one. A trigger that
  // finds the record still in flight is remembered, and that key alone is sent once more when the send settles: a
  // replay is no trigger for other keys, so two failing targets cannot keep replaying each other.
  const flush = useMemo(() => {
    const flushKey = (key: string) => {
      const record = owed.get(key);
      if (record === undefined) return;
      if (sending.get(key) === record) { missed.add(key); return; }
      sending.set(key, record);
      const { kind, id, at } = record;
      const intent = { kind, id, operation: markReadOperation(kind, id, at), optimistic: (state: ReadState) => withMarkedRead(state, kind, id, at) };
      void send.current(intent).then(() => { if (owed.get(key) === record) owed.delete(key); }, () => undefined).finally(() => {
        if (sending.get(key) !== record) return;
        sending.delete(key);
        if (missed.delete(key)) flushKey(key);
      });
    };
    return () => { for (const key of [...owed.keys()]) flushKey(key); };
  }, [owed, sending, missed]);
  const channel = useMemo<ReceiptChannel>(() => ({
    transport, unauthorized,
    markRead(kind, id, activityAt) {
      // Every reader-visible event owes its own acknowledgement, even at an unchanged instant.
      const key = receiptKey(kind, id);
      owed.set(key, { kind, id, at: Math.max(owed.get(key)?.at ?? 0, activityAt ?? 0) });
      flush();
    },
    markUnread(kind, id) {
      owed.delete(receiptKey(kind, id)); // The reader's later intent wins over an acknowledgement still owed.
      return send.current({ kind, id, operation: markUnreadOperation(kind, id), optimistic: (state) => withMarkedUnread(state, kind, id) });
    },
  }), [transport, unauthorized, owed, flush]);
  useEffect(() => onlineManager.subscribe((online) => { if (online) flush(); }), [flush]);
  const connected = useConnectionStatus()?.connected === true;
  useEffect(() => { if (connected) flush(); }, [connected, flush]);
  useEffect(() => client.getQueryCache().subscribe((event) => {
    // A receipts fetch that succeeded. `setQueryData` (this provider's own optimistic and answered writes) dispatches a
    // `manual` success, which is no read: counting it, every send would see itself in flight and replay after failing.
    if (event.type !== 'updated' || event.action.type !== 'success' || event.action.manual === true
      || event.query.queryHash !== hashKey(queryKeys.readReceipts()) || ledger.reconciling) return;
    flush();
  }), [client, ledger, flush]);
  return <ReadReceiptsContext.Provider value={channel}>{children}</ReadReceiptsContext.Provider>;
}

export function readReceiptsQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return { queryKey: queryKeys.readReceipts(), staleTime: 0, refetchOnWindowFocus: true,
    queryFn: async (): Promise<ReadState> => toReadState(await runOperation(transport, readReceiptsOperation(), unauthorized)) };
}

export type ReadReceipts = Readonly<{
  /** Until the kernel's receipts are read nothing is unread: no verdict is not "everything is unread". */
  isUnread(kind: ReadReceiptKind, id: string, activityAt: number | null): boolean;
  /** Owes the reader an acknowledgement of `activityAt`, sent as soon as a write is admitted. */
  markRead(kind: ReadReceiptKind, id: string, activityAt: number | null): void;
  /** The reader's own write: rejects as any other write does (read it through `READ_RECEIPT_FAILURES`). */
  markUnread(kind: ReadReceiptKind, id: string): Promise<void>;
  /** The receipts have been read at least once. */
  ready: boolean;
}>;

function useReceiptChannel(): ReceiptChannel {
  const channel = useContext(ReadReceiptsContext);
  if (channel === null) throw new Error('useReadReceipts needs a ReadReceiptsProvider');
  return channel;
}

export function useReadReceipts(): ReadReceipts {
  const channel = useReceiptChannel();
  const { data } = useQuery(readReceiptsQueryOptions(channel.transport, channel.unauthorized));
  return useMemo<ReadReceipts>(() => ({
    markRead: channel.markRead, markUnread: channel.markUnread, ready: data !== undefined,
    isUnread: (kind, id, activityAt) => data !== undefined && isUnread(data, kind, id, activityAt),
  }), [data, channel]);
}

/**
 * Owes an acknowledgement of what the reader sees: on mount, when `activityAt` moves, on `openedAt` (a repeated
 * selection) and when the tab becomes visible, only while visible. Not on the receipts' data (that would undo a manual unread).
 */
export function useReadReceipt(kind: ReadReceiptKind, id: string | null, activityAt: number | null, enabled = true, openedAt?: number) {
  const { markRead } = useReceiptChannel();
  useEffect(() => {
    if (id === null || !enabled) return;
    const record = () => {
      if (document.visibilityState === 'visible') markRead(kind, id, activityAt);
    };
    record();
    document.addEventListener('visibilitychange', record);
    return () => document.removeEventListener('visibilitychange', record);
  }, [markRead, kind, id, activityAt, enabled, openedAt]);
}
