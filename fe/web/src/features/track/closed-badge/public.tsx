// The track header's closed badge. An open track shows nothing; the activity indicator carries the rest.

import { isClosed } from '../../../../../core/domain/track.ts';
import styles from './closed-badge.module.css';

export type TrackClosedBadgeProps = Readonly<{
  closedAt: number | null;
}>;

export function TrackClosedBadge({ closedAt }: TrackClosedBadgeProps) {
  if (!isClosed({ closedAt })) return null;
  return (
    <span className={styles.host} data-testid="track-closed" role="status" aria-label="Track closed">
      Closed
    </span>
  );
}
