import { hasFailed, isWorking, needsUserAttention, sortAreaTracksByRecent, userVisibleTracks, type Track } from '../../../../core/domain/track.ts';
import type { Area } from '../../../../core/domain/area.ts';

export type SidebarTrackGroups = Readonly<Record<'focus' | 'waiting' | 'pinned' | 'unread' | 'running', readonly Track[]>>;

/** One visible membership projection for desktop groups and mobile shortcuts; receipts stay with their owner. */
export function sidebarTrackGroups(rows: readonly Track[], areas: readonly Area[], isUnread: (track: Track) => boolean): SidebarTrackGroups {
  const tracks = userVisibleTracks(rows, areas);
  const unpinned = tracks.filter((track) => track.pinnedAt === null);
  const failed = sortAreaTracksByRecent(unpinned.filter(hasFailed));
  const attention = sortAreaTracksByRecent(unpinned.filter((track) => !hasFailed(track) && needsUserAttention(track)));
  const runningOnly = sortAreaTracksByRecent(unpinned.filter((track) => !hasFailed(track) && !needsUserAttention(track) && isWorking(track)));
  return {
    focus: [...failed, ...attention, ...runningOnly],
    waiting: tracks.filter((track) => needsUserAttention(track) || hasFailed(track)),
    pinned: tracks.filter((track) => track.pinnedAt !== null).toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0)),
    unread: sortAreaTracksByRecent(tracks.filter(isUnread)),
    running: sortAreaTracksByRecent(tracks.filter(isWorking)),
  };
}
