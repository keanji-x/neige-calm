import { useQueries } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { byRecency, type Conversation } from '../../../../core/domain/conversation.ts';
import { trackDisplayTitle, type Track } from '../../../../core/domain/track.ts';
import { plannerCardIn } from '../../systems/cards/public.js';
import { plannerConversationRow } from '../conversations/planner-row.ts';
import { trackConversationsQueryOptions, trackDetailQueryOptions } from '../providers/queries.ts';

/** Reads only the declared current Track while history is open, using the existing query caches. */
export function useMobileHistoryData(transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
  tracks: readonly Pick<Track, 'id' | 'areaId' | 'title'>[], current: readonly Conversation[], enabled: boolean) {
  const details = useQueries({ queries: tracks.map((track) => ({ ...trackDetailQueryOptions(transport, track.id, unauthorized), enabled })) });
  const lists = useQueries({ queries: tracks.map((track) => ({ ...trackConversationsQueryOptions(transport, track.id, unauthorized), enabled })) });
  const rows = new Map<string, Conversation>();
  tracks.forEach((track, index) => {
    const detail = details[index].data;
    if (detail !== undefined && detail.track.id === track.id) {
      const planner = plannerConversationRow(track.id, trackDisplayTitle(detail.track.title), plannerCardIn(detail.cards));
      if (planner !== null) rows.set(planner.id, planner);
    }
    for (const row of lists[index].data ?? []) {
      if (row.trackId === track.id) rows.set(row.id, { ...row, trackTitle: trackDisplayTitle(track.title) });
    }
  });
  for (const row of current) {
    if (tracks.some((track) => track.id === row.trackId)) rows.set(row.id, row);
  }
  const reads = [...details, ...lists];
  return {
    conversations: [...rows.values()].toSorted(byRecency),
    loading: enabled && reads.some((read) => read.isPending),
    failed: reads.some((read) => read.isError),
    retry: () => { for (const read of reads) void read.refetch(); },
  };
}
