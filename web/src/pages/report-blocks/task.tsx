import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Link } from '@tanstack/react-router';
import type { WaveReportRead } from '../../api/calm';
import type { TaskBlockPayload } from '../../cards/builtins/wave-report';
import { ReportLink, reportUrlTransform } from './index';

export type TaskVerdict = WaveReportRead['taskDiagnostics'][number];
type Diagnostic = TaskVerdict['diagnostics'][number];

function textArg(diagnostic: Diagnostic, key: string): string {
  const value = diagnostic.messageArgs[key];
  return typeof value === 'string' ? value : '';
}

function RelatedBlocks({ diagnostic, waveId }: { diagnostic: Diagnostic; waveId?: string }) {
  const actionLabel = diagnostic.action === 'raise_spec_task_ceiling' ? 'Review capacity' : 'Open related item';
  if (diagnostic.relatedBlockIds.length === 0 && !diagnostic.relatedWaveId) return null;
  return (
    <span className="rb-task-related">
      {' '}{actionLabel}:{' '}
      {diagnostic.relatedBlockIds.map((id, index) => (
        <span key={id}>{index > 0 && ', '}<ReportLink href={`neige://wave/${waveId ?? diagnostic.relatedWaveId ?? ''}#${id}`}>{id}</ReportLink></span>
      ))}
      {diagnostic.relatedWaveId && (
        <>{diagnostic.relatedBlockIds.length > 0 && ', '}<ReportLink href={`neige://wave/${diagnostic.relatedWaveId}`}>referenced wave</ReportLink></>
      )}
    </span>
  );
}

function taskStatusText(status: string | null | undefined, ready: boolean): string {
  switch (status) {
    case 'pending': return 'Waiting to start';
    case 'dispatched': return 'Sent to a worker';
    case 'running': return 'In progress';
    case 'verifying': return 'Running checks';
    case 'done': return 'Completed';
    case 'failed': return 'Needs attention';
    case 'canceled': return 'Canceled';
    default: return ready ? 'Ready to queue' : 'Not queued';
  }
}

function gateResultText(value: unknown): string | null {
  if (typeof value !== 'object' || value === null || !('passed' in value)) return null;
  const gate = value as { passed?: unknown; failing_step?: unknown };
  if (gate.passed === true) return 'Checks: passed';
  if (gate.passed !== false) return null;
  return typeof gate.failing_step === 'string' && gate.failing_step.trim()
    ? `Checks: failed at ${gate.failing_step}`
    : 'Checks: not passed';
}

const taskDiagnosticCopy = Object.freeze({
  duplicate_key: (d: Diagnostic) => `Two task cards use “${textArg(d, 'key')}”. Rename this card or the other one.`,
  dependency_cycle: (d: Diagnostic) => `These tasks wait on each other: ${textArg(d, 'keys')}. Break one dependency to continue.`,
  unknown_dependency: (d: Diagnostic) => `“${textArg(d, 'dependency')}” is not a task card here. It may only exist as an older task row; link this card to a current task key.`,
  gate_required: () => 'This wave requires checks. Add a check, or explain why this task does not need one.',
  spec_task_ceiling: (d: Diagnostic) => d.messageArgs.ceiling === 0
    ? 'The AI-task limit is 0. Raise the limit in wave settings before allowing AI tasks.'
    : `The AI-task limit is ${String(d.messageArgs.ceiling ?? '')}; ${String(d.messageArgs.occupied ?? '')} slots are already in use. Cards are admitted in document order, then by key, so an earlier card can push this one out. Raise the limit in wave settings or move this card earlier.`,
  reference_needs_block: (d: Diagnostic) => `“${textArg(d, 'reference')}” points to a wave, not a block. Link the exact block this task needs.`,
  reference_missing: (d: Diagnostic) => `“${textArg(d, 'reference')}” no longer resolves. Open the destination and link an existing block.`,
  reference_cross_cove: (d: Diagnostic) => `“${textArg(d, 'reference')}” crosses into another cove. Copy the needed context here or link a block in this cove.`,
  reference_chain_too_large: () => 'The reference chain is too deep or wide, so this task is treated as invalid to stay safe. Gather the needed context into fewer blocks.',
  tombstone_blocks_redeclaration: (d: Diagnostic) => `A “do not do” record left by ${textArg(d, 'tombstoned_by') === 'user' ? 'you' : 'the AI'} blocks this task key. Remove that record to allow this key again; restoring automatic AI tasks is separate and keeps the rejection record.`,
  declare_and_wait: () => 'AI-proposed tasks in this wave wait for you. Use “Allow this task” below, or restore automatic AI tasks for the wave.',
  context_stale_reference: () => 'A referenced block changed after work started, so this run can no longer be checked safely. Relink the intended context and create a task with a new key.',
  declaration_changed_in_flight: () => 'This task card changed after work started. The worker output is still available in its card and logs, but it has not been verified. Review the output, then create a task with a new key if needed.',
  context_stale_declaration: () => 'This task card changed after work started. The worker output is still available in its card and logs, but it has not been verified. Review the output, then create a task with a new key if needed.',
  task_key_completed: () => 'This task key has already been delivered. Create a new task card with a new key for more work.',
  invalid_declaration: () => 'This task card is incomplete or invalid. Fix the highlighted task fields, then try again.',
});

export const taskDiagnosticCodes = Object.freeze(Object.keys(taskDiagnosticCopy));

export function taskDiagnosticText(diagnostic: Diagnostic): string {
  const render = taskDiagnosticCopy[diagnostic.code as keyof typeof taskDiagnosticCopy];
  return render ? render(diagnostic) : diagnostic.message;
}

export function ReportTaskBlock({
  payload,
  verdict,
  onRelease,
  onDelete,
  onClearTombstone,
  onRestoreAutomation,
  waveId,
}: {
  payload: TaskBlockPayload;
  verdict?: TaskVerdict;
  onRelease?(): void;
  onDelete?(): void;
  onClearTombstone?(): void;
  onRestoreAutomation?(): void;
  waveId?: string;
}) {
  if (payload.tombstone) {
    const owner = payload.tombstoned_by === 'user' ? 'You left' : 'The AI left';
    const declarer = payload.declared_by === 'user' ? 'You originally proposed it.' : 'The AI originally proposed it.';
    return (
      <section className="rb-task rb-task--readonly" aria-label={`Do not do ${payload.key}`}>
        <strong>{payload.key}</strong>
        <p>{owner} this “do not do” record{payload.tombstone.reason ? `: ${payload.tombstone.reason}` : ''}. {declarer} The AI cannot propose this key again.</p>
        <div className="rb-task-actions">
          <button type="button" onClick={onClearTombstone}>Allow this key again</button>
          <button type="button" onClick={onRestoreAutomation}>Restore automatic AI tasks</button>
        </div>
      </section>
    );
  }
  const delivered = verdict?.status != null && verdict.status !== 'pending';
  const waiting = verdict?.diagnostics.some((d) => d.code === 'declare_and_wait') ?? false;
  const gateText = gateResultText(verdict?.gateResult);
  return (
    <section className={`rb-task ${delivered ? 'rb-task--readonly' : 'rb-task--draft'}`} aria-label={`Task ${payload.key}`}>
      <header><strong>{payload.key}</strong><span>{delivered ? 'Delivered · read only' : 'Draft · editable card'}</span></header>
      <p className="rb-task-meta">{taskStatusText(verdict?.status, payload.ready)}</p>
      <div className="calm-prose rb-task-goal"><ReactMarkdown remarkPlugins={[remarkGfm]} urlTransform={reportUrlTransform} components={{ a: ReportLink }}>{payload.goal}</ReactMarkdown></div>
      {payload.acceptance && <div className="rb-task-acceptance"><span>Done when</span><div className="calm-prose"><ReactMarkdown remarkPlugins={[remarkGfm]} urlTransform={reportUrlTransform} components={{ a: ReportLink }}>{payload.acceptance}</ReactMarkdown></div></div>}
      {payload.depends_on && payload.depends_on.length > 0 && <p>Waits for: {payload.depends_on.join(', ')}</p>}
      {gateText && <p className="rb-task-meta">{gateText}</p>}
      {verdict?.workerCardId && <p><Link to="/wave/$waveId" params={{ waveId: waveId ?? '' }} hash={verdict.workerCardId}>Open worker output</Link></p>}
      {verdict?.diagnostics.map((diagnostic, index) => <p className="rb-task-diagnostic" role="alert" key={`${diagnostic.code}:${index}`}>{taskDiagnosticText(diagnostic)}<RelatedBlocks diagnostic={diagnostic} waveId={waveId} /></p>)}
      <div className="rb-task-actions">
        {waiting && !payload.released_by_user && <button type="button" onClick={onRelease}>Allow this task</button>}
        {!delivered && <button type="button" onClick={onDelete}>Remove task</button>}
        {waiting && <button type="button" onClick={onRestoreAutomation}>Restore automatic AI tasks</button>}
      </div>
    </section>
  );
}
