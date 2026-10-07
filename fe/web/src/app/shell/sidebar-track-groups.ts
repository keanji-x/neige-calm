import { hasFailed, isWorking, needsUserAttention, sortAreaTracksByRecent, type Track } from '../../../../core/domain/track.ts';

export type SidebarTrackGroups = Readonly<Record<'waiting' | 'pinned' | 'unread' | 'running', readonly Track[]>>;

/** Membership and order are shared by desktop groups and mobile shortcuts; callers supply visible Tracks and receipts. */
export function sidebarTrackGroups(tracks: readonly Track[], isUnread: (track: Track) => boolean): SidebarTrackGroups {
  return {
    waiting: tracks.filter((track) => needsUserAttention(track) || hasFailed(track)),
    pinned: tracks.filter((track) => track.pinnedAt !== null).toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0)),
    unread: sortAreaTracksByRecent(tracks.filter(isUnread)),
    running: sortAreaTracksByRecent(tracks.filter(isWorking)),
  };
}
