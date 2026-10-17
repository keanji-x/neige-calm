import styles from './activity-indicator.module.css';

/** Decorative feedback for an already named control. Keep mounted; pause while inactive. */
export function LoadingIndicator({ active }: Readonly<{ active: boolean }>) {
  return <span aria-hidden="true" className={`${styles.indicator} ${styles.working} ${styles.loading}`}
    style={{ animationPlayState: active ? 'running' : 'paused' }} />;
}
