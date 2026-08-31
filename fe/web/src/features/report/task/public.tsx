import type { TaskBlockPayload } from '../../../../../core/domain/report.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './task.module.css';

const DECLARED_BY: Readonly<Record<'spec' | 'user', string>> =
  Object.freeze({ spec: 'Spec agent', user: 'You' });

type WithdrawnTask = Extract<TaskBlockPayload, { tombstoned_by: unknown }>;
type LiveTask = Exclude<TaskBlockPayload, WithdrawnTask>;

function isWithdrawn(payload: TaskBlockPayload): payload is WithdrawnTask {
  return 'tombstoned_by' in payload;
}

export function ReportTaskBlock({ payload, blockId }: {
  payload: TaskBlockPayload;
  blockId: string;
}) {
  if (isWithdrawn(payload)) {
    const reason = payload.tombstone.reason;
    return (
      <details className={styles.task} data-nc-task-state="withdrawn">
        <summary className={styles.head}>
          <span className={styles.marker}><Icon name="chevron-right" size="sm" /></span>
          <span className={styles.kindLabel}>Task</span>
          <span className={styles.key}>{payload.key === '' ? blockId : payload.key}</span>
          <span className={styles.spacer} />
          <span className={styles.withdrawn}>Withdrawn</span>
        </summary>
        {reason !== null && reason !== undefined && reason !== '' && (
          <p className={styles.goal}>{reason}</p>
        )}
        <dl className={styles.fields}>
          <dt className={styles.label}>Declared by</dt>
          <dd className={styles.value}>{DECLARED_BY[payload.declared_by]}</dd>
          <dt className={styles.label}>Withdrawn by</dt>
          <dd className={styles.value}>{DECLARED_BY[payload.tombstoned_by]}</dd>
        </dl>
      </details>
    );
  }

  const live: LiveTask = payload;
  return (
    <details className={styles.task} data-nc-task-state={live.ready ? 'ready' : 'not-ready'}>
      <summary className={styles.head}>
        <span className={styles.marker}><Icon name="chevron-right" size="sm" /></span>
        <span className={styles.kindLabel}>Task</span>
        <span className={styles.key}>{live.key === '' ? blockId : live.key}</span>
        <span className={styles.spacer} />
        <span className={styles.kind}>
          {live.kind}{live.spawn === 'sub-wave' ? ' · sub-wave' : ''}
        </span>
        <span className={live.ready ? styles.ready : styles.notReady}>
          {live.ready ? 'Ready' : 'Not ready'}
        </span>
      </summary>

      <p className={styles.goal}>{live.goal}</p>

      <dl className={styles.fields}>
        {live.acceptance != null && live.acceptance !== '' && (
          <>
            <dt className={styles.label}>Done when</dt>
            <dd className={styles.value}>{live.acceptance}</dd>
          </>
        )}
        {live.gate != null && live.gate.steps.length > 0 && (
          <>
            <dt className={styles.label}>Checks</dt>
            <dd className={styles.value}>
              <ul>
                {live.gate.steps.map((step, index) => (
                  <li key={`${step.name}-${index}`} className={styles.step}>
                    <span className={styles.stepCmd}>{step.cmd}</span>
                  </li>
                ))}
              </ul>
            </dd>
          </>
        )}
        <dt className={styles.label}>Declared by</dt>
        <dd className={styles.value}>{DECLARED_BY[live.declared_by]}</dd>
      </dl>
    </details>
  );
}
