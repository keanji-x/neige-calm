import styles from './error-box.module.css';

/** One recovery action with optional diagnostic disclosure; no request ownership. */
export function ErrorBox({ message, onRetry, actionLabel = 'Retry', description, details, floating = false }: {
  message: string;
  onRetry: () => void;
  actionLabel?: string;
  description?: string;
  details?: string;
  /** Anchor a compact notice above a retained canvas or other resource view. */
  floating?: boolean;
}) {
  const expanded = description !== undefined || details !== undefined || floating;
  return (
    <div role="alert" data-nc-error-box="" className={expanded ? `${styles.recovery} ${floating ? styles.floating : ''}` : undefined}>
      {!expanded && <span className={styles.dot} aria-hidden="true" />}
      <span className={expanded ? styles.reason : undefined}>{message}</span>
      {description !== undefined && <p className={styles.description}>{description}</p>}
      <button type="button" data-nc-action="tertiary" onClick={onRetry}>{actionLabel}</button>
      {details !== undefined && <details className={styles.details}>
        <summary>Details</summary>
        <p>{details}</p>
      </details>}
    </div>
  );
}
