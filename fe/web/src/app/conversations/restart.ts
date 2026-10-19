import { useRef } from 'react';

import {
  DORMANT_NOTICE, recoveryNotice, offersFreshSession, type RunRecovery, RESTART_ACTION, RESTARTED_NOTICE, restartFailureText,
} from '../../../../core/domain/conversation-restart.ts';
import { writeFailureOf } from '../../../../core/domain/failure-class.ts';
import { useState } from '../../ui/state/public.ts';
import { REFILLED_NOTE } from './edit.ts';
import { useConversationCommands, useConversationComposerEmpty, useConversationEditNotice, useConversationRestartOut } from './public.tsx';

/** What the strip above the composer says about the conversation's session, and the action it offers, if any. */
export type RestartStrip = Readonly<{
  tone: 'error' | 'neutral';
  lines: readonly string[];
  /** A failed restart's sentence, said beside the notice it tried to resolve. */
  errors: readonly string[];
  action: string | null;
}>;

/**
 * "Start a fresh session" (#2192) for the open conversation: offered when a send was refused because the session cannot
 * be resumed (`dormant`, held by the registry until the next send) and when the server declares recovery,
 * where Send is disabled. The composer keeps its words and nothing is sent again: the reader presses Send. A restart
 * that was answered leaves its notice in the registry for the card it was made in, whichever is shown by then; its
 * failure is said only while that card is shown.
 */
export function useConversationRestart({ cardId, enabled, recovery, restart }: {
  cardId: string;
  /** Server-owned recovery for the current worker. */
  recovery: RunRecovery | null;
  /** Only the historical contract offers fresh-session recovery. */
  enabled: boolean;
  /** `POST …/planner/restart` for this card, settled once the run state read after it has landed. */
  restart: () => Promise<unknown>;
}) {
  const { noteRestarted, holdRestart } = useConversationCommands();
  const composerEmpty = useConversationComposerEmpty(cardId === '' ? null : cardId);
  const pending = useConversationRestartOut(cardId === '' ? null : cardId);
  const [failure, setFailure] = useState<Readonly<{ cardId: string; message: string }> | null>(null);
  const shownCardId = useRef(cardId);
  shownCardId.current = cardId;
  const notice = useConversationEditNotice(cardId === '' ? null : cardId);
  const error = failure?.cardId === cardId ? failure.message : null;
  const errors = error === null ? [] : [error];
  const offered = enabled && (notice?.kind === 'dormant' || offersFreshSession(recovery));
  const strip: RestartStrip | null = notice?.kind === 'dormant' ? {
    tone: 'error', errors, action: enabled ? RESTART_ACTION : null,
    /* An Edit's replace was refused: a Send now adds a new message, as for any refused replace. */
    lines: notice.edit && !composerEmpty ? [DORMANT_NOTICE, REFILLED_NOTE] : [DORMANT_NOTICE],
  } : recovery !== null && offersFreshSession(recovery) ? { tone: error === null ? 'neutral' : 'error', lines: [recoveryNotice(recovery)], errors, action: enabled ? RESTART_ACTION : null }
    : notice?.kind === 'restarted' ? { tone: 'neutral', lines: [RESTARTED_NOTICE], errors: [], action: null }
    : null;

  const start = () => {
    const startedFor = cardId;
    if (startedFor === '' || !offered) return;
    const held = holdRestart(startedFor, () => restart().then(() => { noteRestarted(startedFor); }, (cause: unknown) => {
      if (shownCardId.current === startedFor) setFailure({ cardId: startedFor, message: restartFailureText(writeFailureOf(cause)) });
    }));
    if (held !== null) setFailure(null);
  };
  return {
    strip,
    pending,
    start,
    /** A send went out: a failed restart's sentence goes, as the notice it was beside does. */
    clearError: () => { setFailure((current) => current?.cardId === cardId ? null : current); },
  };
}
