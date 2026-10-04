import { useLayoutEffect, useMemo, useRef } from 'react';
import { useState } from '../../ui/state/public.ts';
import { stopFailureFeedback, type ConversationStopFeedback } from '../../../../core/domain/conversation-stop.ts';
import { ApiError } from '../providers/queries.ts';

type StopLease = { historyKnown: boolean; newestRowId: number };
type StopView = { cardId: string; request: StopLease | null; canStop: boolean };
type StopNotice = Readonly<{ view: StopView; lease: StopLease; feedback: ConversationStopFeedback }>;

/** One request lease per committed view; a receipt never manufactures completion. */
export function useConversationStop({ cardId, canStop, responseEnded, historyKnown, newestRowId, completedRowId, requestStop }: {
  cardId: string;
  canStop: boolean;
  responseEnded: boolean;
  historyKnown: boolean;
  newestRowId: number;
  completedRowId: number | null;
  requestStop: () => Promise<Readonly<{ stopped: boolean }>>;
}) {
  const view = useMemo<StopView>(() => ({ cardId, request: null, canStop: false }), [cardId]);
  const viewRef = useRef<StopView | null>(null);
  useLayoutEffect(() => {
    viewRef.current = view;
    view.canStop = canStop;
    return () => { if (viewRef.current === view) viewRef.current = null; };
  }, [view, canStop]);
  const [notice, setNotice] = useState<StopNotice | null>(null);
  const owned = notice?.view === view ? notice : null;
  // Compare against every row visible at dispatch: older pages and disappearing
  // terminals cannot establish that the current response ended.
  const terminalAdvanced = owned !== null && owned.lease.historyKnown && historyKnown
    && completedRowId !== null && completedRowId > owned.lease.newestRowId;
  const ended = terminalAdvanced || (owned !== null && view.request === owned.lease && responseEnded);
  useLayoutEffect(() => {
    if (owned === null) return;
    // The first known history establishes a baseline, not completion evidence.
    // The same lease survives receipt updates, so delayed ACKs cannot reset it.
    if (!owned.lease.historyKnown && historyKnown) {
      owned.lease.historyKnown = true;
      owned.lease.newestRowId = newestRowId;
    }
    if (!ended) return;
    if (view.request === owned.lease) view.request = null;
    setNotice((previous) => previous?.lease === owned.lease ? null : previous);
  }, [view, owned, historyKnown, newestRowId, ended]);

  const interrupt = () => {
    if (viewRef.current !== view || !view.canStop || view.request !== null) return;
    const lease: StopLease = { historyKnown, newestRowId };
    view.request = lease;
    const current = () => viewRef.current === view && view.request === lease;
    setNotice({ view, lease, feedback: { kind: 'requesting' } });
    void Promise.resolve().then(requestStop).then((receipt) => {
      if (!current()) return;
      if (!receipt.stopped) view.request = null;
      setNotice({ view, lease, feedback: { kind: receipt.stopped ? 'stopping' : 'unconfirmed' } });
    }).catch((error: unknown) => {
      if (!current()) return;
      view.request = null;
      setNotice({ view, lease, feedback: stopFailureFeedback(error instanceof ApiError ? error.failure : null) });
    });
  };
  const feedback = ended ? null : owned?.feedback ?? null;
  return {
    pending: feedback?.kind === 'requesting' || feedback?.kind === 'stopping',
    feedback,
    interrupt,
    clearFeedback: () => setNotice((previous) => previous?.view === view && previous.lease !== view.request ? null : previous),
  };
}
