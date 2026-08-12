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
          <span className={styles.kindLabel}>Task</span>
          <span className={styles.key}>{payload.key}</span>
          <span className={styles.spacer} />
          <span className={styles.withdrawn}>Withdrawn</span>
        </div>
        {reason !== null && reason !== undefined && reason !== '' && (
          <p className={styles.goal}>{reason}</p>
        )}
        <dl className={styles.fields}>
          <dt className={styles.label}>Declared by</dt>
          <dd className={styles.value}>{DECLARED_BY[payload.declared_by]}</dd>
          <dt className={styles.label}>Withdrawn by</dt>
          <dd className={styles.value}>{DECLARED_BY[payload.tombstoned_by]}</dd>
        </dl>
      </div>
    );
  }

  const live: LiveTask = payload;
  return (
    <div className={styles.task} data-nc-task-state={live.ready ? 'ready' : 'not-ready'}>
      <div className={styles.head}>
        {/* The block says what it is. Every other kind announces itself by its
            own shape — a table is a table — but a task is a paragraph of
            structured text, and without the word it reads as prose that has
            gone strange. */}
        <span className={styles.kindLabel}>Task</span>
        <span className={styles.key}>{live.key}</span>
        <span className={styles.spacer} />
        <span className={styles.kind}>{live.kind}</span>
        {live.spawn === 'sub-wave' && <span className={styles.kind}>sub-wave</span>}
        {/* Ready is a fact about the task, carried in words. It is not a badge:
            §6.6 spends a pill on lifecycle, and a second pill on this page
            would make two different things look like one kind of thing. */}
        <span className={live.ready ? styles.ready : styles.notReady}>
          {live.ready ? 'Ready' : 'Not ready'}
        </span>
      </div>

      <p className={styles.goal}>{live.goal}</p>

      {/*
        Label / value, one grid, one label column — the same shape the rest of
        the app gives a set of facts about one object. It replaces three
        different inline layouts (a run-in label, a two-column list, a bare
        line), which is what made this block look like it had been assembled
        out of whatever was to hand.
      */}
      <dl className={styles.fields}>
        {live.acceptance !== undefined && live.acceptance !== '' && (
          <>
            <dt className={styles.label}>Done when</dt>
            <dd className={styles.value}>{live.acceptance}</dd>
          </>
        )}
        {live.gate !== undefined && live.gate.steps.length > 0 && (
          <>
            <dt className={styles.label}>Checks</dt>
            <dd className={styles.value}>
              <ul>
                {live.gate.steps.map((step) => (
                  <li key={step.name} className={styles.step}>
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
    </div>
  );
}
