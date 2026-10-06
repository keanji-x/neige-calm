import type { ReactNode } from 'react';
import type { ConversationTurnOutcome } from '../../../../../core/domain/conversation.ts';
import type { ConversationStopFeedback } from '../../../../../core/domain/conversation-stop.ts';
import type { ConversationMetaClock, RunningTurnAnchor } from '../../../../../core/domain/conversation-meta.ts';
import { ThreadStatusNotice, type CopyResponseAction, type EditAction, type ResponseAction } from './status-notice.tsx';
import { useRunningElapsedMs } from './running-clock.ts';
import styles from './thread.module.css';

/** One plain sentence for the `codexErrorInfo` values a reader can act on; every other code is shown as the token codex sent. */
const FAILURE_HINTS: Readonly<Record<string, string>> = Object.freeze({
  contextWindowExceeded: 'The conversation no longer fits in the model’s context window.',
  usageLimitExceeded: 'The usage limit for this account has been reached.',
  rateLimitExceeded: 'Requests are being rate-limited; try again in a moment.',
  serverOverloaded: 'The model provider is overloaded; try again in a moment.',
});

/** One stable row across live, request, pause and terminal transitions. */
export function CurrentStatusNotice({ outcome, canContinue, live, statusUnconfirmed, statusLoading, stalled, stalledReason, feedback, copyAction, editAction, regenerateAction, runningAnchor }: {
  outcome: ConversationTurnOutcome | null;
  canContinue: boolean;
  live: boolean;
  statusUnconfirmed: boolean;
  statusLoading: boolean;
  stalled: boolean;
  stalledReason: string | null;
  feedback: ConversationStopFeedback | null;
  copyAction: CopyResponseAction | null;
  editAction: EditAction | null;
  regenerateAction: ResponseAction | null;
  /** Where the running turn's clock starts; `null` shows `Running` with no number. */
  runningAnchor: RunningTurnAnchor | null;
}) {
  const runningElapsedMs = useRunningElapsedMs(!statusUnconfirmed && !stalled && feedback === null && live ? runningAnchor?.startMs ?? null : null);
  let heading: string;
  let tone: 'neutral' | 'warning' | 'error' = 'neutral';
  let details: ReactNode;
  let clock: ConversationMetaClock = { elapsedMs: null, timestamp: null };
  let terminal: ConversationTurnOutcome['status'] | undefined;
  if (statusUnconfirmed) {
    heading = 'Status unconfirmed'; tone = 'warning';
    details = <>
      <p className={styles.outcomeReason}>{statusLoading
        ? 'Checking the conversation’s current state.'
        : 'The conversation’s current state has not been confirmed.'}</p>
      {feedback !== null && <>
        <p className={styles.outcomeReason}>{stopFeedbackHeading(feedback)}</p>
        <StopFeedbackDetails feedback={feedback} />
      </>}
      {outcome !== null && <>
        <p className={styles.outcomeReason}>Last recorded response: {outcome.status}.</p>
        <OutcomeDetails outcome={outcome} canContinue={false} />
      </>}
    </>;
  } else if (stalled) {
    heading = 'Paused'; tone = 'warning';
    details = <p className={styles.outcomeReason}>{stalledReason ?? 'This conversation is stuck.'}</p>;
  } else if (feedback !== null) {
    heading = stopFeedbackHeading(feedback);
    tone = feedback.kind === 'failed' ? 'error' : 'neutral';
    details = <StopFeedbackDetails feedback={feedback} />;
  } else if (live) {
    heading = 'Running';
    clock = { elapsedMs: runningElapsedMs, timestamp: null };
  }
  else if (outcome !== null) {
    terminal = outcome.status;
    heading = terminal === 'completed' ? 'Completed' : terminal === 'interrupted' ? 'Interrupted' : 'Failed';
    tone = terminal === 'completed' ? 'neutral' : terminal === 'interrupted' ? 'warning' : 'error';
    clock = { elapsedMs: outcome.elapsedMs, timestamp: { kind: 'finished', atMs: outcome.atMs } };
    if (terminal !== 'completed') details = <OutcomeDetails outcome={outcome} canContinue={canContinue} />;
  } else return null;
  return <ThreadStatusNotice heading={heading} tone={tone} clock={clock} outcome={terminal} copyAction={copyAction} editAction={editAction} regenerateAction={regenerateAction}>{details}</ThreadStatusNotice>;
}

/** Request feedback is evidence about Stop, independent of whether the current run can be read. */
function stopFeedbackHeading(feedback: ConversationStopFeedback): string {
  return feedback.kind === 'requesting' ? 'Requesting stop' : feedback.kind === 'stopping' ? 'Stopping'
    : feedback.kind === 'failed' ? 'Stop failed' : 'Stop unconfirmed';
}

function StopFeedbackDetails({ feedback }: { feedback: ConversationStopFeedback }) {
  const reason = feedback.kind === 'failed' ? feedback.message
    : feedback.kind === 'requesting' ? 'Waiting for the stop request to finish.'
    : feedback.kind === 'stopping' ? 'Waiting for the response to end.'
    : 'The stop may not have taken effect: the response may still be running or may already have ended.';
  return <p className={styles.outcomeReason}>{reason}</p>;
}

/** Recorded diagnostics stay readable even when a run query cannot confirm current execution. */
function OutcomeDetails({ outcome, canContinue }: { outcome: ConversationTurnOutcome; canContinue: boolean }) {
  if (outcome.status === 'completed') return null;
  const hasReason = outcome.text !== undefined && outcome.text.trim() !== '';
  const hint = outcome.status === 'failed' ? outcomeHintText(outcome.code, outcome.rawStatus) : null;
  return <>
    {hasReason && <p className={styles.outcomeReason} data-nc-turn-outcome-message="" title={outcome.message}>{outcome.text}</p>}
    {hint !== null && <p className={styles.outcomeReason} data-nc-turn-outcome-hint="">{hint}</p>}
    {!hasReason && hint === null && <p className={styles.outcomeReason} data-nc-turn-outcome-fallback="">
      {outcome.status === 'failed' ? 'No failure details are available.' : 'No interruption details are available.'}
    </p>}
    {canContinue && <p className={styles.outcomeGuidance} data-nc-interruption-guidance="">Send a message to continue.</p>}
  </>;
}

function outcomeHintText(code: string | undefined, rawStatus: string | undefined): string | null {
  const hint = code === undefined || code.trim() === '' ? null : (FAILURE_HINTS[code] ?? code);
  return hint ?? (rawStatus === undefined ? null : `Ended with status “${rawStatus}”`);
}
