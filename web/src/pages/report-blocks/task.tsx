import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { WaveReportRead } from '../../api/calm';
import type { TaskBlockPayload } from '../../cards/builtins/wave-report';
import { ReportLink, reportUrlTransform } from './index';

export type TaskVerdict = WaveReportRead['taskDiagnostics'][number];
type Diagnostic = TaskVerdict['diagnostics'][number];

function textArg(diagnostic: Diagnostic, key: string): string {
  const value = diagnostic.messageArgs[key];
  return typeof value === 'string' ? value : '';
}

function RelatedBlocks({ diagnostic }: { diagnostic: Diagnostic }) {
  if (diagnostic.relatedBlockIds.length === 0) return null;
  return (
    <span className="rb-task-related">
      {' '}See{' '}
      {diagnostic.relatedBlockIds.map((id, index) => (
        <span key={id}>{index > 0 && ', '}<a href={`#${id}`}>{id}</a></span>
      ))}
    </span>
  );
}

export function taskDiagnosticText(diagnostic: Diagnostic): string {
  const arg = (key: string) => textArg(diagnostic, key);
  switch (diagnostic.code) {
    case 'duplicate_key': return `Two task cards use “${arg('key')}”. Rename this card or the other one.`;
    case 'dependency_cycle': return `These tasks wait on each other: ${arg('keys')}. Break one dependency to continue.`;
    case 'unknown_dependency': return `“${arg('dependency')}” is not a task card here. It may only exist as an older task row; link this card to a current task key.`;
    case 'gate_required': return 'This wave requires checks. Add a check, or explain why this task does not need one.';
    case 'spec_task_ceiling': return `The AI-task limit is ${String(diagnostic.messageArgs.ceiling ?? '')}; ${String(diagnostic.messageArgs.occupied ?? '')} slots are already in use. Cards are admitted in document order, then by key, so an earlier card can push this one out. Raise the limit in wave settings or move this card earlier.`;
    case 'reference_needs_block': return `“${arg('reference')}” points to a wave, not a block. Link the exact block this task needs.`;
    case 'reference_missing': return `“${arg('reference')}” no longer resolves. Open the destination and link an existing block.`;
    case 'reference_cross_cove': return `“${arg('reference')}” crosses into another cove. Copy the needed context here or link a block in this cove.`;
    case 'reference_chain_too_large': return 'The reference chain is too deep or wide, so this task is treated as invalid to stay safe. Gather the needed context into fewer blocks.';
    case 'tombstone_blocks_redeclaration': return 'A “do not do” record blocks this task key. Remove that record to allow this key again; restoring automatic AI tasks is separate and keeps the rejection record.';
    case 'declare_and_wait': return 'AI-proposed tasks in this wave wait for you. Use “Allow this task” below, or restore automatic AI tasks for the wave.';
    case 'context_stale_reference': return 'A referenced block changed after work started, so this run can no longer be checked safely. Relink the intended context and create a task with a new key.';
    case 'declaration_changed_in_flight':
    case 'context_stale_declaration': return 'This task card changed after work started. The worker output is still available in its card and logs, but it has not been verified. Review the output, then create a task with a new key if needed.';
    case 'task_key_completed': return 'This task key has already been delivered. Create a new task card with a new key for more work.';
    default: return diagnostic.message;
  }
}

export function ReportTaskBlock({
  payload,
  verdict,
  onRelease,
  onDelete,
  onClearTombstone,
  onRestoreAutomation,
}: {
  payload: TaskBlockPayload;
  verdict?: TaskVerdict;
  onRelease?(): void;
  onDelete?(): void;
  onClearTombstone?(): void;
  onRestoreAutomation?(): void;
}) {
  if (payload.tombstone) {
    return (
      <section className="rb-task rb-task--readonly" aria-label={`Do not do ${payload.key}`}>
        <strong>{payload.key}</strong>
        <p>Do not do{payload.tombstone.reason ? `: ${payload.tombstone.reason}` : ''}. The AI cannot propose this key again.</p>
        <div className="rb-task-actions">
          <button type="button" onClick={onClearTombstone}>Allow this key again</button>
          <button type="button" onClick={onRestoreAutomation}>Restore automatic AI tasks</button>
        </div>
      </section>
    );
  }
  const delivered = verdict?.status != null && verdict.status !== 'pending';
  const waiting = verdict?.diagnostics.some((d) => d.code === 'declare_and_wait') ?? false;
  return (
    <section className={`rb-task ${delivered ? 'rb-task--readonly' : 'rb-task--draft'}`} aria-label={`Task ${payload.key}`}>
      <header><strong>{payload.key}</strong><span>{delivered ? 'Delivered · read only' : 'Draft · editable card'}</span></header>
      <p className="rb-task-meta">{verdict?.status ?? (payload.ready ? 'Ready to queue' : 'Not queued')}{verdict?.statusDetail ? ` · ${verdict.statusDetail}` : ''}</p>
      <div className="calm-prose rb-task-goal"><ReactMarkdown remarkPlugins={[remarkGfm]} urlTransform={reportUrlTransform} components={{ a: ReportLink }}>{payload.goal}</ReactMarkdown></div>
      {payload.acceptance && <div className="rb-task-acceptance"><span>Done when</span><div className="calm-prose"><ReactMarkdown remarkPlugins={[remarkGfm]} urlTransform={reportUrlTransform} components={{ a: ReportLink }}>{payload.acceptance}</ReactMarkdown></div></div>}
      {payload.depends_on && payload.depends_on.length > 0 && <p>Waits for: {payload.depends_on.join(', ')}</p>}
      {verdict?.gateResult != null && <p className="rb-task-meta">Checks: {JSON.stringify(verdict.gateResult)}</p>}
      {verdict?.workerCardId && <p><a href={`#${verdict.workerCardId}`}>Open worker output</a></p>}
      {verdict?.diagnostics.map((diagnostic, index) => <p className="rb-task-diagnostic" role="alert" key={`${diagnostic.code}:${index}`}>{taskDiagnosticText(diagnostic)}<RelatedBlocks diagnostic={diagnostic} /></p>)}
      <div className="rb-task-actions">
        {waiting && !payload.released_by_user && <button type="button" onClick={onRelease}>Allow this task</button>}
        {!delivered && <button type="button" onClick={onDelete}>Remove task</button>}
        {waiting && <button type="button" onClick={onRestoreAutomation}>Restore automatic AI tasks</button>}
      </div>
    </section>
  );
}
