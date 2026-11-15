import type { ReactNode, ComponentProps } from 'react';
import { ProseBlock } from '../document/content.tsx';
import { boundedStatusDetail, type ReportTaskRow, type TaskBlockPayload } from '../../../../../core/domain/report.ts';
import { providerPresentation, taskProvider } from '../../../../../core/domain/provider-presentation.ts';
import { selectionSourceLabel } from '../../../../../core/domain/task-selection.ts';
import { taskStatusPhrase } from '../../../../../core/view/track-page.ts';
import { useState } from '../../../ui/state/public.ts';
import { SpringRotation } from '../../../ui/motion/rotation.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './task.module.css';

// The KEY stays `spec` while the LABEL says Planner: `declared_by` / `tombstoned_by` are report-block DSL values
// written by agents into stored documents, so the value is frozen; the human-facing label follows the rename.
const DECLARED_BY: Readonly<Record<'spec' | 'user', string>> =
  Object.freeze({ spec: 'Planner agent', user: 'You' });

type WithdrawnTask = Extract<TaskBlockPayload, { tombstoned_by: unknown }>;
type LiveTask = Exclude<TaskBlockPayload, WithdrawnTask>;

function isWithdrawn(payload: TaskBlockPayload): payload is WithdrawnTask {
  return 'tombstoned_by' in payload;
}

export function ReportTaskBlock({ payload, blockId, task, renderExecution, textContext }: {
  textContext?: Omit<ComponentProps<typeof ProseBlock>, 'markdown' | 'blockId' | 'compact'>;
  payload: TaskBlockPayload;
  blockId: string;
  task?: ReportTaskRow;
  renderExecution?: (task: ReportTaskRow, expanded: boolean) => ReactNode;
}) {
  const [expanded, setExpanded] = useState(false);
  const [visited, setVisited] = useState(false);
  const toggle = (open: boolean) => { setExpanded(open); if (open) setVisited(true); };
  if (isWithdrawn(payload)) {
    const reason = payload.tombstone.reason;
    return (
      <details onToggle={event => { toggle(event.currentTarget.open); }} className={styles.task} data-nc-task-state="withdrawn">
        <summary className={styles.head}>
          <SpringRotation className={styles.marker} angle={expanded ? 90 : 0}><Icon name="chevron-right" size="sm" /></SpringRotation>
          <span className={styles.kindLabel}>Task</span>
          <span className={styles.key}>{payload.key === '' ? blockId : payload.key}</span>
          <span className={styles.spacer} />
          <span className={styles.withdrawn}>Withdrawn</span>
        </summary>
        {visited && <>{reason !== null && reason !== undefined && reason !== '' && (
          <div className={styles.goal}><ProseBlock markdown={reason} blockId={null} compact {...textContext} /></div>
        )}
        <dl className={styles.fields}>
          <dt className={styles.label}>Declared by</dt>
          <dd className={styles.value}>{DECLARED_BY[payload.declared_by]}</dd>
          <dt className={styles.label}>Withdrawn by</dt>
          <dd className={styles.value}>{DECLARED_BY[payload.tombstoned_by]}</dd>
        </dl></>}
      </details>
    );
  }

  const live: LiveTask = payload;
  const worker = live.kind === 'compact' ? null : live;
  const selection = task?.execution?.workerSelection;
  const provider = selection === undefined ? taskProvider(live.kind, 'provider' in live ? live.provider : undefined) : selection.provider;
  const requestedModel = selection === undefined ? ('model' in live ? live.model : null) : selection.requested_model;
  const requestedEffort = selection === undefined ? ('reasoning_effort' in live ? live.reasoning_effort : null) : selection.requested_effort;
  const status = task?.execution?.status ?? task?.status ?? null;
  const explanation = [
    status === null ? null : taskStatusPhrase(task?.execution?.label ?? status, task?.execution === undefined ? task?.statusDetail ?? null : boundedStatusDetail(task.execution.statusDetail)),
    task?.execution === undefined ? task?.pendingReason?.message : task.execution.blockingReason,
  ].filter(Boolean).join(' — ');
  return (
    <details onToggle={(event) => { toggle(event.currentTarget.open); }} className={styles.task} data-nc-task-state={status ?? (live.ready ? 'ready' : 'not-ready')}>
      <summary className={styles.head}>
        <SpringRotation className={styles.marker} angle={expanded ? 90 : 0}><Icon name="chevron-right" size="sm" /></SpringRotation>
        <span className={styles.kindLabel}>Task</span>
        <span className={styles.key}>{live.key === '' ? blockId : live.key}</span>
        <span className={styles.spacer} />
        <span className={styles.kind}>
          {provider === null ? live.kind : providerPresentation(provider)?.label ?? provider}{worker?.spawn === 'sub-track' ? ' · sub-track' : ''}
        </span>
        <span className={styles.state} title={explanation || undefined}>
          {task?.execution?.label ?? status ?? (live.ready ? 'Declaration ready' : 'Declaration not ready')}
        </span>
      </summary>

      {visited && <>{task !== undefined && live.key !== '' && renderExecution?.(task, expanded)}
      {worker !== null && <div className={styles.goal}>
        {worker.kind === 'terminal' ? worker.command : <ProseBlock markdown={worker.goal} blockId={null} compact {...textContext} />}
      </div>}

      <dl className={styles.fields}>
        {provider !== null && <>
          <dt className={styles.label}>Mechanism</dt><dd className={styles.value}>{selection?.mechanism ?? live.kind}</dd>
          <dt className={styles.label}>Provider</dt><dd className={styles.value}>{providerPresentation(provider)?.label ?? provider}</dd>
          <dt className={styles.label}>Requested model / effort</dt><dd className={styles.value}>{requestedModel ?? 'Provider default'} / {requestedEffort ?? 'Provider default'}</dd>
          <dt className={styles.label}>Effective model</dt><dd className={styles.value}>{selection?.effective_model ?? '未记录'} · {selectionSourceLabel(selection?.selection_source?.model)}</dd>
          <dt className={styles.label}>Effective effort</dt><dd className={styles.value}>{selection?.effective_effort ?? '未记录'} · {selectionSourceLabel(selection?.selection_source?.effort)}</dd>
          {selection?.cleanup_pending === true && <><dt className={styles.label}>Cleanup</dt><dd className={styles.value}>{selection.turn_phase === 'unknown' ? 'Unknown · cleanup pending' : 'Awaiting native round settlement'}</dd></>}
        </>}
        {worker?.acceptance != null && worker.acceptance !== '' && (
          <>
            <dt className={styles.label}>Done when</dt>
            <dd className={styles.value}><ProseBlock markdown={worker.acceptance} blockId={null} compact {...textContext} /></dd>
          </>
        )}
        {worker?.gate != null && worker.gate.steps.length > 0 && (
          <>
            <dt className={styles.label}>Checks</dt>
            <dd className={styles.value}>
              <ul>
                {worker.gate.steps.map((step, index) => (
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
      </dl></>}
    </details>
  );
}
