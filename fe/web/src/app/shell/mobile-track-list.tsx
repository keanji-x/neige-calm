import { useId } from 'react';
import type { Track } from '../../../../core/domain/track.ts';
import { isClosed, trackActivityState, trackDisplayTitle } from '../../../../core/domain/track.ts';
import { activityLabelOf, activityNameBit } from '../../../../core/domain/activity.ts';
import { TrackTitle } from '../../features/track/title/public.tsx';
import { TrackActions, type TrackActionsProps } from '../../features/track/row/actions.tsx';
import { ActivityIndicator } from '../../ui/activity-indicator/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { MobileList, MobileListEmpty } from '../../ui/mobile-list/public.tsx';
import styles from './mobile-navigation.module.css';

/** The same Track row, actions and activity semantics in workspace navigation and sidebar shortcuts. */
export function MobileTrackList({ tracks, currentTrackId, onOpenTrack, isUnread, trackActions, emptyMessage }: Readonly<{
  tracks: readonly Track[];
  currentTrackId: string | undefined;
  onOpenTrack: (trackId: string) => void;
  isUnread: (track: Track) => boolean;
  trackActions?: (track: Track) => Omit<TrackActionsProps, 'track' | 'className'>;
  emptyMessage?: string;
}>) {
  const listId = useId();
  return <MobileList className={styles.navigationList}>
          {tracks.map((track) => {
            /* The same state the rail row shows: the kernel's activity overlay plus this reader's receipt, never the open/closed state.
               The indicator is decorative here; `unread` is the button's description, never part of its name. Each list owns its description IDs, including when another list is mounted behind the drawer. */
            const activity = trackActivityState(track, isUnread(track));
            const activityBit = activityNameBit(activity);
            const descriptionId = `${listId}-track-${track.id}-unread`;
            return <li key={track.id} className={styles.trackRow}>
              <button type="button" className={styles.track} aria-label={`${trackDisplayTitle(track.title)}${activityBit ? `, ${activityBit}` : ''}${isClosed(track) ? ', closed' : ''}`}
                aria-describedby={activity === 'unread' ? descriptionId : undefined}
                aria-current={track.id === currentTrackId ? 'page' : undefined} onClick={() => onOpenTrack(track.id)}>
                <span className={styles.trackIcon}><Icon name="file" /></span>
                <span className={styles.trackCopy}><span className={styles.trackTitle}><TrackTitle track={track} /></span><time className={styles.trackMeta} dateTime={new Date(track.updatedAt).toISOString()}>{new Date(track.updatedAt).toLocaleDateString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit' }).replaceAll('/', '-')} 更新</time>{isClosed(track) && <span className={styles.trackMeta}>Closed</span>}</span>
                {activity !== 'quiet' && <span className={styles.trackActivity} aria-hidden="true"><ActivityIndicator state={activity} /></span>}
                {trackActions === undefined && <span className={styles.trackArrow}><Icon name="chevron-right" /></span>}
              </button>
              {trackActions !== undefined && <TrackActions track={track} {...trackActions(track)} className={styles.trackActions} />}
              {activity === 'unread' && <span hidden id={descriptionId}>{activityLabelOf('unread')}</span>}
            </li>;
          })}
    {tracks.length === 0 && emptyMessage !== undefined && <MobileListEmpty>{emptyMessage}</MobileListEmpty>}
  </MobileList>;
}
