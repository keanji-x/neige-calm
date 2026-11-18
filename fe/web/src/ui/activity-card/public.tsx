import type { ReactNode } from 'react';
import { CardFrame } from '../card-frame/public.tsx';
import styles from './activity-card.module.css';

/** Presentational card shell for metadata, captured activity and a compact input form. */
export function ActivityCard({ header, details, activity, activityLabel, controls }: Readonly<{
  header: ReactNode; details: ReactNode; activity: ReactNode; activityLabel: string; controls: ReactNode;
}>) {
  return <CardFrame className={styles.frame}>
    {header}<div className={styles.body}>
      <div className={styles.metadata}>{details}</div>
      <div className={styles.flow} aria-label={activityLabel}>{activity}</div>
      <div className={styles.controls}>{controls}</div>
    </div>
  </CardFrame>;
}
