import { hasFailed, isWorking, needsUserAttention, sortAreaTracksByRecent, userVisibleTracks, type Track } from '../../../../core/domain/track.ts';
import type { Area } from '../../../../core/domain/area.ts';

export type SidebarTrackGroups = Readonly<Record<'waiting' | 'pinned' | 'unread' | 'running', readonly Track[]>>;

/** One visible membership projection for desktop groups and mobile shortcuts; receipts stay with their owner. */
export function sidebarTrackGroups(rows: readonly Track[], areas: readonly Area[], isUnread: (track: Track) => boolean): SidebarTrackGroups {
  const tracks = userVisibleTracks(rows, areas);
  return {
    waiting: tracks.filter((track) => needsUserAttention(track) || hasFailed(track)),
    pinned: tracks.filter((track) => track.pinnedAt !== null).toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0)),
    unread: sortAreaTracksByRecent(tracks.filter(isUnread)),
    running: sortAreaTracksByRecent(tracks.filter(isWorking)),
  };
}
