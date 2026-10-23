import { useQueries } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { byRecency, type Conversation } from '../../../../core/domain/conversation.ts';
import { trackDisplayTitle, type Track } from '../../../../core/domain/track.ts';
import { trackConversationRows } from '../conversations/planner-row.ts';
import { trackConversationsQueryOptions, trackDetailQueryOptions } from '../providers/queries.ts';

/** Reads only the declared current Track while history is open, using the existing query caches. */
export function useMobileHistoryData(transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
  tracks: readonly Pick<Track, 'id' | 'areaId' | 'title'>[], current: readonly Conversation[], enabled: boolean) {
  const details = useQueries({ queries: tracks.map((track) => ({ ...trackDetailQueryOptions(transport, track.id, unauthorized), enabled })) });
  const lists = useQueries({ queries: tracks.map((track) => ({ ...trackConversationsQueryOptions(transport, track.id, unauthorized), enabled })) });
  const rows = new Map<string, Conversation>();
  for (const row of current) {
    if (tracks.some(track => track.id === row.trackId)) rows.set(row.id, row);
  }
  tracks.forEach((track, index) => {
    const detail = details[index].data;
    const listed = lists[index].data ?? [];
    const projected = detail !== undefined && detail.track.id === track.id
      ? trackConversationRows(track.id, trackDisplayTitle(track.title), detail.cards, listed)
      : listed.map(row => ({ ...row, trackTitle: trackDisplayTitle(track.title) }));
    // Current rows are a fallback only while detail is unavailable, never a second role authority.
    if (detail !== undefined) for (const [id, row] of rows) if (row.trackId === track.id) rows.delete(id);
    for (const row of projected) if (row.trackId === track.id && (detail !== undefined || !rows.has(row.id))) rows.set(row.id, row);
  });
  const reads = [...details, ...lists];
  return {
    conversations: [...rows.values()].toSorted(byRecency),
    loading: enabled && reads.some((read) => read.isPending),
    failed: reads.some((read) => read.isError),
    retry: () => { for (const read of reads) void read.refetch(); },
  };
}
