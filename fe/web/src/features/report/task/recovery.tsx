import type { ReactNode } from 'react';
import { attemptStatusLabel, type TaskAttempt, type TaskRecoveryView } from '../../../../../core/domain/task-recovery.ts';
import styles from './task.module.css';

export type TaskRecoveryProps = Readonly<{
  view: TaskRecoveryView | undefined;
  loading: boolean;
  loadError: string | null;
  busy: boolean;
  error: string | null;
  accepted: boolean;
  retryUncertain: boolean;
  onRefresh: () => void;
  onRecover: (() => void) | undefined;
  openWorker: ((cardId: string) => void) | undefined;
  openableWorkerIds: ReadonlySet<string>;
}>;

/** Rendering only: capability and request ownership belong to app/core. */
export function TaskRecoveryDetails({ view, loading, loadError, busy, error, accepted, retryUncertain,
  onRefresh, onRecover, openWorker, openableWorkerIds }: TaskRecoveryProps) {
  return <section className={styles.recovery} aria-label="Task execution">
    {loading && view === undefined && <p role="status">Loading execution history…</p>}
    {loadError !== null && <p role="alert">Could not refresh execution history: {loadError}</p>}
    {view !== undefined && <>
      <p className={styles.current}>Current attempt {view.current.generation} · {attemptStatusLabel(view.current.status)}</p>
      {view.current.status_detail !== null && <p className={styles.detail}>{view.current.status_detail}</p>}
      {view.current.status === 'failed' && <p className={styles.detail}>{view.recovery.reason}</p>}
    </>}
    {busy && <p role="status">Requesting recovery…</p>}
    {accepted && <p role="status">Recovery requested. A new attempt is queued for preparation.</p>}
    {error !== null && <p role="alert" className={styles.detail}>{error}</p>}
    <div className={styles.actions}>
      {onRecover !== undefined && <button type="button" className={styles.action} disabled={busy}
        onClick={onRecover}>{retryUncertain ? 'Retry recovery request' : 'Recover task'}</button>}
      {!loading && <button type="button" className={styles.action} onClick={onRefresh}>Refresh execution history</button>}
    </div>
    {view !== undefined && <details className={styles.history}>
      <summary>Attempt history ({view.attempts.length})</summary>
      <ol className={styles.attempts}>
        {view.attempts.map((attempt) => <li key={attempt.attempt_id}>
          <details>
            <summary>Attempt {attempt.generation} · {attemptStatusLabel(attempt.status)}
              {attempt.attempt_id === view.current.attempt_id ? ' · Current' : ''}</summary>
            <AttemptEvidence attempt={attempt}>
              {attempt.worker_card_id !== null && openWorker !== undefined
                && openableWorkerIds.has(attempt.worker_card_id)
                && <button type="button" className={styles.action} onClick={() => { openWorker(attempt.worker_card_id!); }}>
                  Open attempt {attempt.generation}
                </button>}
            </AttemptEvidence>
          </details>
        </li>)}
      </ol>
    </details>}
  </section>;
}

function AttemptEvidence({ attempt, children }: { attempt: TaskAttempt; children: ReactNode }) {
  return <div className={styles.detail}>
    <p>Created <time dateTime={new Date(attempt.created_at_ms).toISOString()}>{new Date(attempt.created_at_ms).toLocaleString()}</time></p>
    {attempt.finished_at_ms !== null && <p>Finished <time dateTime={new Date(attempt.finished_at_ms).toISOString()}>
      {new Date(attempt.finished_at_ms).toLocaleString()}</time></p>}
    {attempt.status_detail !== null && <p>{attempt.status_detail}</p>}
    {children}
  </div>;
}
