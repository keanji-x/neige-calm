import { useLayoutEffect, useMemo, useRef } from 'react';
import { useState } from '../../ui/state/public.ts';
import type { ConversationStopFeedback } from '../../../../core/domain/conversation-stop.ts';

type StopView = { cardId: string; request: object | null; canStop: boolean };
type StopNotice = Readonly<{
  view: StopView;
  feedback: ConversationStopFeedback;
  historyKnown: boolean;
  completedId: string | null;
}>;

/** One request lease per conversation view; a receipt never manufactures completion. */
export function useConversationStop({ cardId, canStop, historyKnown, completedId, requestStop, failureText }: {
  cardId: string;
  canStop: boolean;
  historyKnown: boolean;
  completedId: string | null;
  requestStop: () => Promise<Readonly<{ stopped: boolean }>>;
  failureText: (error: unknown) => string;
}) {
  const view = useMemo<StopView>(() => ({ cardId, request: null, canStop: false }), [cardId]);
  const viewRef = useRef<StopView | null>(null);
  // Ownership follows committed views, not speculative renders. Unmounting
  // retires callbacks without cancelling a stop the user already requested.
  useLayoutEffect(() => {
    viewRef.current = view;
    view.canStop = canStop;
    return () => { if (viewRef.current === view) viewRef.current = null; };
  }, [view, canStop]);
  const [notice, setNotice] = useState<StopNotice | null>(null);
  const owned = notice?.view === view ? notice : null;
  // New authoritative terminal activity retires the old action notice. This is
  // presentation only: it neither changes send authority nor infers who stopped it.
  const ended = owned !== null && owned.historyKnown && historyKnown && owned.completedId !== completedId;
  const feedback = ended ? null : owned?.feedback ?? null;

  const interrupt = () => {
    if (viewRef.current !== view || !view.canStop || view.request !== null) return;
    const request = {};
    view.request = request;
    const current = () => viewRef.current === view && view.request === request;
    setNotice({ view, feedback: { kind: 'requesting' }, historyKnown, completedId });
    void Promise.resolve().then(requestStop).then((receipt) => {
      if (!current()) return;
      setNotice(receipt.stopped ? null : { view, feedback: { kind: 'unconfirmed' }, historyKnown, completedId });
    }).catch((error: unknown) => {
      if (!current()) return;
      setNotice({ view, feedback: { kind: 'failed', message: failureText(error) }, historyKnown, completedId });
    }).finally(() => {
      if (current()) view.request = null;
    });
  };
  return {
    pending: owned?.feedback.kind === 'requesting',
    feedback,
    interrupt,
    clearFeedback: () => setNotice((previous) => previous?.view === view && previous.feedback.kind !== 'requesting' ? null : previous),
  };
}
