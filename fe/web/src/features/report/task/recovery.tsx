import type { CurrentTaskExecution } from '../../../../../core/domain/task-execution.ts';
import type { ReactNode } from 'react';
import { attemptStatusLabel, type TaskAttempt, type TaskRecoveryView } from '../../../../../core/domain/task-recovery.ts';
import { Button } from '@astryxdesign/core/Button';
import { ErrorBox } from '../../../ui/error-box/public.tsx';
import styles from './task.module.css';

export type TaskRecoveryProps = Readonly<{
  view: TaskRecoveryView | undefined;
  current: CurrentTaskExecution | undefined;
  loading: boolean;
  loadError: string | null;
  onRefresh: () => void;
  openWorker: ((cardId: string) => void) | undefined;
  openableWorkerIds: ReadonlySet<string>;
}>;

/** Rendering only: request ownership belongs to app/core. */
export function TaskRecoveryDetails({ view, current, loading, loadError,
  onRefresh, openWorker, openableWorkerIds }: TaskRecoveryProps) {
  return <section className={styles.recovery} aria-label="Task execution">
    {loading && view === undefined && <p role="status">Loading execution history…</p>}
    {loadError !== null && <ErrorBox message={`Could not refresh execution history: ${loadError}`}
      onRetry={onRefresh} actionLabel="Refresh execution history" pending={loading} />}
    {view?.current === null && current === undefined && <p>No attempts yet</p>}
    {current !== undefined && <>
      <p className={styles.current}>{`Current attempt ${current.generation} · ${current.label}`}</p>
      {current.statusDetail !== null && <p className={styles.detail}>{current.statusDetail}</p>}
      {current.blockingReason !== null && <p className={styles.detail}>{current.blockingReason}</p>}
    </>}
    {loadError === null && <div className={styles.actions}>
      <Button type="button" variant="secondary" size="sm" label="Refresh execution history"
        isLoading={loading} onClick={onRefresh} />
    </div>}
    {view !== undefined && <details className={styles.history}>
      <summary>Attempt history ({view.attempts.length})</summary>
      <ol className={styles.attempts}>
        {view.attempts.map((attempt) => <li key={attempt.attempt_id}>
          <details>
            <summary>Attempt {attempt.generation} · {attemptStatusLabel(attempt.status)}
              {attempt.attempt_id === current?.attemptId ? ' · Current' : ''}</summary>
            <AttemptEvidence attempt={attempt}>
              {attempt.worker_card_id !== null && openWorker !== undefined
                && openableWorkerIds.has(attempt.worker_card_id)
                && <Button type="button" variant="secondary" size="sm" label={`Open attempt ${attempt.generation}`}
                  onClick={() => { openWorker(attempt.worker_card_id!); }} />}
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
