import { useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import { createContext, useContext, useEffect, useMemo, type ReactNode } from 'react';
import type { ApiOperation, ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  isUnread, markReadOperation, markUnreadOperation, readReceiptsOperation, toReadState, withMarkedRead, withMarkedUnread,
  withReceipt, type ReadReceipt, type ReadReceiptKind, type ReadState,
} from '../../../../core/domain/read-receipts.ts';
import { queryKeys, runOperation } from './queries.ts';

/**
 * The kernel's read receipts (#2596), read through one query and written optimistically. Owner: the
 * root route, which mounts {@link ReadReceiptsProvider} with the app's transport; every reader below it
 * uses {@link useReadReceipts}. Writes emit no event, so the query refetches on focus and after each write.
 */
type ReceiptChannel = Readonly<{ transport: ApiTransportPort; unauthorized: UnauthorizedChannel }>;

const ReadReceiptsContext = createContext<ReceiptChannel | null>(null);

export function ReadReceiptsProvider({ transport, unauthorized, children }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; children: ReactNode;
}>) {
  const channel = useMemo(() => ({ transport, unauthorized }), [transport, unauthorized]);
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
  /** Acknowledges `activityAt`; writes only when the target is unread now. */
  markRead(kind: ReadReceiptKind, id: string, activityAt: number | null): void;
  markUnread(kind: ReadReceiptKind, id: string): void;
  /** The receipts have been read at least once. */
  ready: boolean;
}>;

async function write(
  client: QueryClient, channel: ReceiptChannel, operation: ApiOperation<ReadReceipt>, optimistic: (state: ReadState) => ReadState,
): Promise<void> {
  const queryKey = queryKeys.readReceipts();
  // Cancel only a refetch over existing data: cancelling the first read would leave the query with nothing to show.
  if (client.getQueryData<ReadState>(queryKey) !== undefined) {
    await client.cancelQueries({ queryKey });
    client.setQueryData<ReadState>(queryKey, (state) => state === undefined ? state : optimistic(state));
  }
  try {
    const receipt = await runOperation(channel.transport, operation, channel.unauthorized);
    client.setQueryData<ReadState>(queryKey, (state) => state === undefined ? state : withReceipt(state, receipt));
  } catch {
    // The read below puts the kernel's receipt back in place of the optimistic one.
  } finally {
    void client.invalidateQueries({ queryKey });
  }
}

export function useReadReceipts(): ReadReceipts {
  const channel = useContext(ReadReceiptsContext);
  if (channel === null) throw new Error('useReadReceipts needs a ReadReceiptsProvider');
  const client = useQueryClient();
  const { data } = useQuery(readReceiptsQueryOptions(channel.transport, channel.unauthorized));
  // The writes read the cache when called, so they stay the same functions while the data moves.
  const writes = useMemo(() => ({
    markRead(kind: ReadReceiptKind, id: string, activityAt: number | null) {
      const current = client.getQueryData<ReadState>(queryKeys.readReceipts());
      if (current === undefined || !isUnread(current, kind, id, activityAt)) return;
      const at = activityAt ?? 0;
      void write(client, channel, markReadOperation(kind, id, at), (state) => withMarkedRead(state, kind, id, at));
    },
    markUnread(kind: ReadReceiptKind, id: string) {
      void write(client, channel, markUnreadOperation(kind, id), (state) => withMarkedUnread(state, kind, id));
    },
  }), [client, channel]);
  return useMemo<ReadReceipts>(() => ({
    ...writes,
    ready: data !== undefined,
    isUnread: (kind, id, activityAt) => data !== undefined && isUnread(data, kind, id, activityAt),
  }), [data, writes]);
}

/**
 * Acknowledges what the reader sees: on mount, once the receipts are read, when `activityAt` moves,
 * on `openedAt` (a repeated selection) and when the tab becomes visible. A background tab never
 * acknowledges work the reader has not seen.
 */
export function useReadReceipt(kind: ReadReceiptKind, id: string | null, activityAt: number | null, enabled = true, openedAt?: number) {
  const { markRead, ready } = useReadReceipts();
  useEffect(() => {
    if (id === null || !enabled || !ready) return;
    const markVisible = () => {
      if (document.visibilityState === 'visible') markRead(kind, id, activityAt);
    };
    markVisible();
    document.addEventListener('visibilitychange', markVisible);
    return () => document.removeEventListener('visibilitychange', markVisible);
    // Not on the receipts' data: re-running on a manual unread of the open target would undo it at once.
  }, [markRead, ready, kind, id, activityAt, enabled, openedAt]);
}
