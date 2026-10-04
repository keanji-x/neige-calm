import styles from './placeholder.module.css';

/** The three states a live reference shows instead of its data. */
export type LivePlaceholderProps =
  | { state: 'detached'; source: string; caption?: string | null }
  | { state: 'pending'; source: string; caption?: string | null }
  | { state: 'unavailable'; source: string; reason: string; caption?: string | null };

/** One placeholder for every live reference in a report: live tables and view slots. */
export function LivePlaceholder(props: LivePlaceholderProps) {
  const { caption, source } = props;
  return (
    <div className={styles.placeholder}>
      {caption != null && caption !== '' && <p className={styles.text}>{caption}</p>}
      {props.state === 'detached' && <p className={styles.text}>This view does not carry live data.</p>}
      {props.state === 'pending' && <p className={styles.text}>Waiting for {source} — nothing has been pushed here yet.</p>}
      {props.state === 'unavailable' && <p className={styles.text} role="status">{source} cannot be displayed: {props.reason}.</p>}
    </div>
  );
}
