// The `task` block — a unit of work the spec agent declared inside the report.
//
// **Read-only, and that is the design (§8.3), not a missing slice.** The
// legacy card carried Release / Delete / Restore buttons; those are writes to
// the wave's task graph, and a report is an account of what happened, not the
// console you drive it from. What this renders is exactly what the block
// declares: who asked for it, whether it is ready to queue, what it is for,
// and what would count as done.
//
// A withdrawn task keeps its row. It renders `Withdrawn` with both
// attributions intact — the task existed, other reports may cite its block id,
// and a document that silently dropped it would be lying about its own past.

import type { TaskBlockPayload } from '../../../../../core/domain/report.ts';
import styles from './task.module.css';

const DECLARED_BY: Readonly<Record<'spec' | 'user', string>> =
  Object.freeze({ spec: 'Spec agent', user: 'You' });

type WithdrawnTask = Extract<TaskBlockPayload, { tombstoned_by: unknown }>;
type LiveTask = Exclude<TaskBlockPayload, WithdrawnTask>;

/** `tombstoned_by` is the discriminant, not `tombstone`: a live task may carry
 *  an explicit `tombstone: null`, so presence of that key proves nothing. */
function isWithdrawn(payload: TaskBlockPayload): payload is WithdrawnTask {
  return 'tombstoned_by' in payload;
}

export function ReportTaskBlock({ payload }: { payload: TaskBlockPayload }) {
  if (isWithdrawn(payload)) {
    const reason = payload.tombstone.reason;
    return (
      <div className={styles.task} data-nc-task-state="withdrawn">
        <div className={styles.head}>
          <span className={styles.key}>{payload.key}</span>
          <span className={styles.withdrawn}>Withdrawn</span>
        </div>
        <p className={styles.meta}>
          Declared by {DECLARED_BY[payload.declared_by]}
          {' · '}
          Withdrawn by {DECLARED_BY[payload.tombstoned_by]}
        </p>
        {reason !== null && reason !== undefined && reason !== '' && (
          <p className={styles.goal}>{reason}</p>
        )}
      </div>
    );
  }

  const live: LiveTask = payload;
  return (
    <div className={styles.task} data-nc-task-state={live.ready ? 'ready' : 'not-ready'}>
      <div className={styles.head}>
        <span className={styles.key}>{live.key}</span>
        <span className={styles.kind}>{live.kind}</span>
        {live.spawn === 'sub-wave' && <span className={styles.kind}>sub-wave</span>}
      </div>
      <p className={styles.meta}>
        Declared by {DECLARED_BY[live.declared_by]}
        {' · '}
        {/* Ready is a fact about the task, carried in words. It is not a badge:
            §6.6 spends a pill on lifecycle, and a second pill on this page
            would make two different things look like one kind of thing. */}
        <span className={live.ready ? styles.ready : styles.notReady}>
          {live.ready ? 'Ready' : 'Not ready'}
        </span>
      </p>
      <p className={styles.goal}>{live.goal}</p>
      {live.acceptance !== undefined && live.acceptance !== '' && (
        <p className={styles.acceptance}>
          <span className={styles.label}>Done when</span>
          {live.acceptance}
        </p>
      )}
      {live.gate !== undefined && live.gate.steps.length > 0 && (
        <ul className={styles.gate}>
          {live.gate.steps.map((step) => (
            <li key={step.name} className={styles.step}>
              <span className={styles.stepName}>{step.name}</span>
              <span className={styles.stepCmd}>{step.cmd}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
