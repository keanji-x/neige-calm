import styles from './path-label.module.css';

/** Read-only path label; owns no lifecycle, state, or external resources. */
export function PathLabel({ label, path }: { label: string; path: string }) {
  return (
    <div className={styles.pathLabel}>
      <span>{label}</span>
      <code>{path}</code>
    </div>
  );
}
