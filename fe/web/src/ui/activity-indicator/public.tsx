import styles from './activity-indicator.module.css';

export type ActivityState = 'failed' | 'attention' | 'working' | 'unread' | 'quiet';

/** The owning control supplies the accessible label; this is its visual counterpart. */
export function ActivityIndicator({ state }: Readonly<{ state: ActivityState }>) {
  if (state === 'quiet') return null;
  return <span className={`${styles.indicator} ${styles[state]}`} data-nc-activity={state} aria-hidden="true" />;
}
