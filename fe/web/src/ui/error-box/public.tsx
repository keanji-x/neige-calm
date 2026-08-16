import styles from './error-box.module.css';

export function ErrorBox({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <div role="alert" data-nc-error-box="">
      <span className={styles.dot} aria-hidden="true" />
      <span>{message}</span>
      <button type="button" data-nc-action="tertiary" onClick={onRetry}>Retry</button>
    </div>
  );
}
