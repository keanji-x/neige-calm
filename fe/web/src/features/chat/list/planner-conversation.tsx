// A Planner row's "New conversation" (#2774) and an earlier conversation's read-only notice: presentational, the
// request and its navigation are the caller's.

import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { conversationName, type Conversation } from '../../../../../core/domain/conversation.ts';
import { PLANNER_HISTORY_NOTICE, PLANNER_NEW_CONVERSATION_COPY as COPY } from '../../../../../core/domain/planner-conversation.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './list.module.css';

/**
 * Start a new, blank conversation on a Planner row. `start` rejects with the one line to show; `held` names the rows
 * whose own turn, queue or send is in flight, which disables the action as a hint only: the server decides.
 */
export type PlannerNewConversation = Readonly<{
  start: (cardId: string) => Promise<void>;
  held: ReadonlySet<string>;
}>;

/** One row's confirmation, request and refusal; the item is disabled while its request is out. */
export function usePlannerNewConversation(conversation: Conversation, action: PlannerNewConversation | undefined, working: boolean) {
  const [asking, setAsking] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const confirm = async () => {
    setAsking(false);
    if (pending || action === undefined) return;
    setPending(true); setError(null);
    try { await action.start(conversation.id); }
    catch (failure) { setError(failure instanceof Error ? failure.message : 'The new conversation was not started.'); }
    finally { setPending(false); }
  };
  return {
    disabled: pending || working || (action?.held.has(conversation.id) ?? false),
    error,
    ask: () => { setAsking(true); },
    dialog: <ConfirmDialog open={asking} title={COPY.title} description={<p>{COPY.description}</p>}
      confirmLabel={COPY.confirm} cancelLabel={COPY.cancel} destructive={false}
      onConfirm={() => { void confirm(); }} onCancel={() => { setAsking(false); }} />,
  };
}

/** The menu entry a row's existing actions menu offers. */
export function PlannerNewConversationItem({ disabled, onAsk }: Readonly<{ disabled: boolean; onAsk: () => void }>) {
  return <DropdownMenuItem label={COPY.action} isDisabled={disabled} onClick={onAsk} />;
}

/** The refusal line, as archive refusals are said. */
export function PlannerNewConversationError({ error }: Readonly<{ error: string | null }>) {
  return error === null ? null : <p role="alert" className={styles.archiveError}>{error}</p>;
}

/**
 * For desktop list rows and mobile history: the same `more` menu the archive action uses. `beforeAsk` lets a host
 * that is itself modal (mobile history) step aside before the confirmation opens.
 */
export function PlannerConversationAction({ conversation, action, working = false, className, beforeAsk }: Readonly<{
  conversation: Conversation;
  action: PlannerNewConversation;
  working?: boolean;
  className?: string;
  beforeAsk?: (ask: () => void) => void;
}>) {
  const { disabled, error, ask: askNow, dialog } = usePlannerNewConversation(conversation, action, working);
  const ask = beforeAsk === undefined ? askNow : () => { beforeAsk(askNow); };
  return <>
    <div className={className}>
      <DropdownMenu placement="below" alignment="end" button={{
        label: `Actions for conversation: ${conversationName(conversation)}`, icon: <Icon name="more" size="sm" />,
        isIconOnly: true, variant: 'ghost', size: 'sm',
      }}>
        <PlannerNewConversationItem disabled={disabled} onAsk={ask} />
      </DropdownMenu>
    </div>
    <PlannerNewConversationError error={error} />
    {dialog}
  </>;
}

/** Where an earlier conversation's composer would be: it is history, with nothing to restore. */
export function PlannerHistoryNotice() {
  return <div className={styles.archiveNotice}><p>{PLANNER_HISTORY_NOTICE}</p></div>;
}
