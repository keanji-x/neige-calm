import { isClosed, trackDisplayTitle, type Track } from '../../../../../core/domain/track.ts';
import styles from './title.module.css';

/** Track title text only. Hosts own typography, truncation, navigation and editing. */
export function TrackTitle({ track }: Readonly<{
  track: Pick<Track, 'title' | 'closedAt'>;
}>) {
  return <span className={isClosed(track) ? styles.closed : undefined}>
    {trackDisplayTitle(track.title)}
  </span>;
}
