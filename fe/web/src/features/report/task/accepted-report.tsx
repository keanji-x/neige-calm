import type { AcceptedTaskReport } from '../../../../../core/domain/independent-task.ts';
import styles from './task.module.css';

/** Model content is text/JSON only. Artifact strings are evidence, never file links. */
export function AcceptedReport({ value, loading, terminal, error, onRefresh }: {
  terminal: boolean; value: AcceptedTaskReport | undefined; loading: boolean; error: string | null; onRefresh: () => void;
}) {
  return <section className={styles.history} aria-label="Accepted task report">
    <h4>Accepted report</h4>
    {loading && value === undefined && <p role="status">Loading accepted report…</p>}
    {error !== null && <p role="alert">Could not load accepted report: {error}</p>}
    {value?.report === null && <p>{terminal ? 'This attempt ended without an accepted report.' : 'No accepted report yet.'}</p>}
    {value?.report?.kind === 'failed' && <p className={styles.detail}>Task failed: {value.report.reason}</p>}
    {value?.report?.kind === 'completed' && <>
      <pre className={styles.result}>{typeof value.report.result === 'string' ? value.report.result : JSON.stringify(value.report.result, null, 2)}</pre>
      {value.report.artifacts.length > 0 && <>
        <p>Reported artifacts</p>
        <ul>{value.report.artifacts.map((artifact, index) => <li className={styles.detail} key={index}>{artifact}</li>)}</ul>
      </>}
    </>}
    <button className={styles.action} type="button" disabled={loading} onClick={onRefresh}>Refresh accepted report</button>
  </section>;
}
