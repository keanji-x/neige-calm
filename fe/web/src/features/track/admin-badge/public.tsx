// The track header's Area admin badge: shown while the server reports an admin grant, nothing otherwise.

import styles from './admin-badge.module.css';

export type TrackAdminBadgeProps = Readonly<{
  /** `admin_granted_at` from the server; `null` while the Track is not an Area admin. */
  adminGrantedAt: number | null;
}>;

export function TrackAdminBadge({ adminGrantedAt }: TrackAdminBadgeProps) {
  if (adminGrantedAt === null) return null;
  return (
    <span className={styles.host} data-testid="track-admin" role="status" aria-label="Area admin">
      Admin
    </span>
  );
}
