import { useEffect, useMemo, useRef } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { Banner } from '@astryxdesign/core/Banner';
import type { ConversationSubmissionDelivery, ConversationActionUnavailableReason } from '../../../../../core/api/generated/wire.ts';
import { useState } from '../../../ui/state/public.ts';
import { SizeMotion } from '../../../ui/motion/size.tsx';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';

type ResolutionProps = Readonly<{
  keepInput?: (submission: ConversationSubmissionDelivery) => Promise<void>;
  retryInput?: (submission: ConversationSubmissionDelivery) => Promise<void>;
}>;
/** Canonical status only; it never adds a transcript or outbox row. */
export function HeldInputStatus({ held, ...actions }: ResolutionProps & Readonly<{ held: readonly ConversationSubmissionDelivery[] }>) {
  if (held.length === 0) return null;
  return <SizeMotion motionKey={held.map(submission => `${submission.submission_id}:${submission.attempt?.revision}`).join(',')}>
    {held.map(submission => <HeldInput key={submission.submission_id} submission={submission} {...actions} />)}
  </SizeMotion>;
}
function unavailable(reason: ConversationActionUnavailableReason): string {
  switch (reason) {
    case 'already_kept': return 'This input was kept as delivered.';
    case 'already_resolved': return 'This input already has a Human decision.';
    case 'not_held': return 'This input is no longer held.';
    case 'prior_invocation_not_recorded': return 'Retry is unavailable because the original model settings were not recorded.';
    case 'invocation_not_fully_frozen': return 'Retry requires an explicitly recorded model and effort.';
    case 'attachments_not_frozen': return 'Retry is unavailable for inputs with attachments.';
    case 'erased_or_withdrawn': return 'Retry is unavailable because original input was erased or withdrawn.';
    case 'unsupported_binding': return 'Retry requires the original Codex binding and exact native thread.';
    case 'unsupported_source': return 'Retry is unavailable for this input source.';
  }
}
function HeldInput({ submission, keepInput, retryInput }: ResolutionProps & Readonly<{ submission: ConversationSubmissionDelivery }>) {
  const [sending, setSending] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const resolution = submission.attempt?.resolution;
  const status = useRef<HTMLDivElement>(null);
  const retryAnchor = useRef<HTMLSpanElement>(null);
  const origin = useMemo(() => ({kind:'anchor' as const,anchor:{ref:retryAnchor},unavailable:'surface-center' as const}), []);
  const restoreFocus = useRef(false);
  useEffect(() => {
    if (resolution && restoreFocus.current) {
      const node = status.current;
      if (node !== null && (document.activeElement === document.body || node.contains(document.activeElement))) node.focus();
      restoreFocus.current = false;
    }
  }, [resolution]);
  const invocation = submission.attempt?.invocation;
  const original = invocation?.kind === 'recorded'
    ? `Original model: ${invocation.configuration.model ?? 'inherited from the thread'}; effort: ${invocation.configuration.effort ?? 'inherited from the thread'}; permissions: ${invocation.configuration.permission}.`
    : 'The original model settings were not recorded.';
  const resolve = async (action: typeof keepInput) => {
    if (action === undefined || sending) return;
    restoreFocus.current = true;
    setSending(true); setError(null);
    try { await action(submission); setConfirming(false); }
    catch (failure) { setError(failure instanceof Error ? failure.message : 'Could not resolve this input. Try again.'); }
    finally { setSending(false); }
  };
  const retry = submission.attempt?.actions.retry;
  return <div ref={status} role="status" tabIndex={-1} data-nc-held-input={submission.submission_id}>
    <Banner status={resolution ? 'info' : 'warning'} title={resolution?.kind === 'retry' ? 'Retry queued' : resolution ? 'Input kept as delivered' : 'Delivery not confirmed'}
      description={resolution ? (resolution.result.waiting_for_settlement ? 'Further input waits for the original thread to settle.' : 'The original execution remains tracked until it ends.') : `This input may have reached the model. ${original} Keep treats it as delivered.`} />
    {submission.attempt?.actions.keep.kind === 'available' && keepInput !== undefined
      ? <Button label={sending ? 'Saving…' : 'Keep as delivered'} isDisabled={sending || confirming} onClick={() => { void resolve(keepInput); }} /> : null}
    {retry?.kind === 'available' && retryInput !== undefined
      ? <span ref={retryAnchor}><Button label="Retry" isDisabled={sending} onClick={() => { setError(null); setConfirming(true); }} /></span>
      : !resolution && retry?.kind === 'unavailable' ? <p>{unavailable(retry.reason)}</p> : null}
    <ConfirmDialog origin={origin} open={confirming} title="Retry this input?" confirmLabel="Send again" confirmBusyLabel="Sending…"
      description={<><p>This input may already have been delivered. Sending it again can repeat its effects.</p>{error === null ? null : <p role="alert">{error}</p>}</>}
      destructive={false} confirmState={sending ? 'busy' : 'ready'}
      onCancel={() => { if (!sending) setConfirming(false); }} onConfirm={() => { void resolve(retryInput); }} />
    {!confirming && error !== null ? <p role="alert">{error}</p> : null}
  </div>;
}
