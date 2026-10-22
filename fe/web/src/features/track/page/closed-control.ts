import { useCallback, useEffect, useRef } from 'react';
import { writeFailureText } from '../../../../../core/domain/failure-class.ts';
import { TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT } from '../../../../../core/domain/track.ts';
import { useState } from '../../../ui/state/public.ts';
import { useCommittedCallback } from '../../../ui/state/committed-callback.ts';
import { useOperationFeedback, type OperationFeedbackState } from '../../../ui/operation-feedback/public.tsx';

export type TrackClosedControl = Readonly<{
  pending: boolean;
  feedback: OperationFeedbackState;
  setClosed: (action: 'close' | 'reopen') => Promise<boolean>;
}>;

/** Shared by the Track menu and Planner commands, including the post-PATCH refetch fence. */
export function useTrackClosedControl({ trackId, canReopen, canClose, patch }: Readonly<{
  trackId: string;
  canReopen: boolean;
  canClose: boolean;
  patch: (closed: boolean) => Promise<unknown>;
}>): TrackClosedControl {
  const offered = canReopen ? 'reopen' : canClose ? 'close' : null;
  const feedback = useOperationFeedback();
  const [pending, setPending] = useState(false);
  const pendingFrom = useRef<Readonly<{ from: 'close' | 'reopen' }> | null>(null);
  const release = useCallback((token: NonNullable<typeof pendingFrom.current>) => {
    if (pendingFrom.current !== token) return;
    pendingFrom.current = null;
    setPending(false);
  }, [setPending]);
  useEffect(() => {
    // Refetched capability releases its current holder; network success alone does not.
    const token = pendingFrom.current;
    if (token === null || token.from === offered) return;
    release(token);
  }, [offered, release]);
  const setClosed = useCommittedCallback(trackId, async (action: 'close' | 'reopen') => {
    if (offered !== action || pendingFrom.current !== null) return false;
    const token = { from: action };
    pendingFrom.current = token;
    setPending(true);
    let failure = '';
    const read = writeFailureText(TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT[action]);
    const done = await feedback.run(Promise.resolve().then(() => patch(action === 'close')), reason => {
      const text = read(reason);
      failure = text ?? '';
      return text;
    });
    if (!done) {
      release(token);
      throw new Error(failure);
    }
    return true;
  });
  return { pending, feedback, setClosed };
}
