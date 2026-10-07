import { hashKey, useQuery, useQueryClient, type InfiniteData } from '@tanstack/react-query';
import { useCallback, useEffect, useMemo, useRef, useSyncExternalStore } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { HarnessLiveReplies, HarnessPhaseTag } from '../../../../core/api/generated/wire.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ConversationTurn } from '../../../../core/domain/conversation.ts';
import {
  LIVE_REPLY_POLL_MS, awaitsSettling, liveReplyTurns, reconcileLiveReplies, replyMayStream,
  type LiveReplyCopy, type LiveReplyObservation, type LiveReplyTranscriptRow,
} from '../../../../core/domain/conversation-live.ts';
import { cancelThenInvalidate } from '../events/query-refresh.ts';
import { harnessLiveQueryOptions } from '../providers/queries.ts';
import { numberReads } from './read-order.ts';
import { useState } from '../../ui/state/public.ts';

const NO_COPIES: readonly LiveReplyCopy[] = Object.freeze([]);

type HeldCopies = Readonly<{ cardId: string; read: number; copies: readonly LiveReplyCopy[]; lastReply: HarnessLiveReplies | null }>;

/** A page of transcript rows, as far as numbering its read needs it. */
type TranscriptPage = readonly LiveReplyTranscriptRow[];
type TranscriptResult = InfiniteData<TranscriptPage, number>;
type TranscriptQueryOptions = Readonly<{ queryFn: (context: { pageParam: number }) => Promise<TranscriptPage> }>;

/**
 * The order the transcript's reads started in (#1923 S2, rule b). Each page read is numbered as it
 * starts, from the tab's one read order, and a result carries the number of the read its newest page
 * came from. A result this view did not read carries 0, which no copy retires by.
 */
export type TranscriptReads = Readonly<{
  /** The transcript query, with its page reads numbered and its results stamped. */
  track: <Options extends TranscriptQueryOptions>(options: Options) => Options & {
    structuralSharing: (previous: unknown, next: unknown) => unknown;
  };
  startOf: (result: Readonly<{ pages: readonly TranscriptPage[] }> | undefined) => number;
  /** The number of the latest read started so far. */
  latest: () => number;
}>;

/** The transcript reads of one conversation view, held for its lifetime; `nextRead` numbers each as it starts. */
export function useTranscriptReads(nextRead: () => number): TranscriptReads {
  const [reads] = useState(() => createTranscriptReads(nextRead));
  return reads;
}

function createTranscriptReads(nextRead: () => number): TranscriptReads {
  /* Keyed by a result's newest page: Load earlier keeps that page, and so its number. */
  const { queryFn, structuralSharing, startOf, latest } = numberReads<TranscriptPage, Readonly<{ pages: readonly TranscriptPage[] }>>(
    nextRead, (result) => result.pages[0]);
  return { track: (options) => ({ ...options, queryFn: queryFn(options.queryFn), structuralSharing }), startOf, latest };
}

/**
 * The open conversation's streamed replies (#1923 S2), as agent turns for the transcript's tail.
 * Polls `GET …/harness/live` only while this card is mounted and a reply may stream; the query
 * layer's recovery gating applies as it does to every read. The copies are held per card, so a
 * switch never shows one card's text in another.
 */
export function useLiveReplies({ transport, unauthorized, cardId, enabled, phase, runningTurnId, nextRead, transcriptKey, transcriptReads, items }: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  cardId: string;
  enabled: boolean;
  phase: HarnessPhaseTag | null;
  /** Confirmed run identity, or null while no running turn is known. */
  runningTurnId: string | null;
  /** Tab-owned read order; each streaming visit receives a unique boundary. */
  nextRead: () => number;
  /** The key of the transcript query the rows come from: its results are read and re-read here. */
  transcriptKey: readonly unknown[];
  /** The numbering that query's reads were tracked with. */
  transcriptReads: TranscriptReads;
  /** Every loaded transcript row. */
  items: readonly LiveReplyTranscriptRow[];
}): readonly ConversationTurn[] {
  const client = useQueryClient();
  const streaming = enabled && replyMayStream(phase);
  const scope = useStreamReadScope(cardId, streaming, phase, runningTurnId, nextRead);
  const liveOptions = harnessLiveQueryOptions(transport, cardId, unauthorized);
  const live = useQuery({
    ...liveOptions,
    // A delayed initial read cannot hold up or answer a later streaming visit.
    // Prefix invalidation still reaches the card; cached payload shape is unchanged.
    queryKey: [...liveOptions.queryKey, scope.read, scope.turnId],
    enabled: streaming,
    refetchInterval: LIVE_REPLY_POLL_MS,
    gcTime: 0,
  });
  const [held, setHeld] = useState<HeldCopies>(() => ({ cardId, read: scope.read, copies: NO_COPIES, lastReply: null }));
  const copies = held.cardId === cardId && held.read === scope.read ? held.copies : NO_COPIES;
  /* Leaving a card forgets its copies, so returning to it never brings back text from before. */
  useEffect(() => {
    setHeld((current) => current.cardId === cardId && current.read === scope.read
      ? current : { cardId, read: scope.read, copies: NO_COPIES, lastReply: null });
  }, [cardId, scope.read]);
  const apply = useCallback((observation: LiveReplyObservation) => {
    setHeld((current) => {
      const base = current.cardId === cardId && current.read === scope.read ? current.copies : NO_COPIES;
      const next = reconcileLiveReplies(base, observation);
      return next === base && current.cardId === cardId && current.read === scope.read
        ? current : { cardId, read: scope.read, copies: next,
          lastReply: current.cardId === cardId && current.read === scope.read ? current.lastReply : null };
    });
  }, [cardId, scope.read]);
  /* Which read the stored transcript's newest page came from. Numbered rather than timed: a read
     started in the same millisecond as a phase observation would otherwise be ambiguous. */
  const readTranscriptStart = useCallback(
    () => transcriptReads.startOf(client.getQueryData<TranscriptResult>(transcriptKey)),
    [client, transcriptKey, transcriptReads],
  );
  const transcriptHash = useMemo(() => hashKey(transcriptKey), [transcriptKey]);
  const subscribeToQueries = useCallback((notify: () => void) => client.getQueryCache().subscribe((event) => {
    if (event.query.queryHash === transcriptHash) notify();
  }), [client, transcriptHash]);
  const transcriptStart = useSyncExternalStore(subscribeToQueries, readTranscriptStart);

  // Confirmation of the first identity keeps matching text; a different identity
  // retires the previous turn immediately, even if an idle phase was not observed.
  useEffect(() => {
    if (scope.turnId !== null) apply({ kind: 'active-turn', turnId: scope.turnId });
  }, [apply, scope.turnId]);
  const reply = live.data;
  // Consume each cache value before committing children, instead of committing
  // the old body once and then the grown body in a passive effect. The cursor
  // survives transcript retirement so a completed item is not re-added forever.
  if (reply !== undefined && scope.streaming && (held.cardId !== cardId || held.read !== scope.read || held.lastReply !== reply)) {
    setHeld({ cardId, read: scope.read, lastReply: reply,
      copies: reconcileLiveReplies(copies, { kind: 'poll', reply, atMs: Date.now(), activeTurnId: scope.turnId }) });
  }

  /* Drawn from the copies the current transcript does not retire, so a stored row and its live
     copy never share a frame; the effect below forgets the retired ones. */
  const visible = useMemo(
    () => {
      const current = scope.turnId === null ? copies
        : reconcileLiveReplies(copies, { kind: 'active-turn', turnId: scope.turnId });
      return reconcileLiveReplies(current, { kind: 'transcript', items, readStart: transcriptStart });
    },
    [copies, items, transcriptStart, scope.turnId],
  );

  /* Only a read started after this point stands for the phase, however late an earlier one lands.
     The transcript is re-read here so that one always starts; the cancel comes first because the
     query layer would hand back an initial read still in flight in place of a new one. */
  const rereadAfter = useRef<number | null>(null);
  const reread = useCallback(() => {
    rereadAfter.current = transcriptReads.latest();
    void cancelThenInvalidate(client, transcriptKey);
  }, [client, transcriptKey, transcriptReads]);
  useEffect(() => {
    if (phase === null || !awaitsSettling(visible, phase)) return;
    apply({ kind: 'phase', phase, latestReadStart: transcriptReads.latest() });
    reread();
  }, [apply, phase, reread, transcriptReads, visible]);
  /* A read started after the re-read can cancel it (Load earlier does) and keep the stored newest page,
     so while a settled copy still shows, an idle transcript is read again until a later read lands.
     Not after a failed read: the query layer's retries are spent by then, and starting another here
     would retry it for as long as the conversation stays open. The next event or refetch resumes. */
  const transcriptLanded = useSyncExternalStore(subscribeToQueries, () => {
    const state = client.getQueryState(transcriptKey);
    return state?.fetchStatus === 'idle' && state.status === 'success';
  });
  const awaitsRead = visible.some((copy) => copy.settledAt !== null);
  useEffect(() => {
    const after = rereadAfter.current;
    if (!transcriptLanded || !awaitsRead || after === null || transcriptReads.latest() <= after) return;
    reread();
  }, [awaitsRead, reread, transcriptLanded, transcriptReads]);
  useEffect(() => {
    if (visible !== copies) apply({ kind: 'transcript', items, readStart: transcriptStart });
  }, [apply, copies, items, transcriptStart, visible]);

  return useMemo(() => liveReplyTurns(visible), [visible]);
}


/** A view's streaming read window, not another server execution state. Keep the
 * last confirmed identity through an interrupt; losing an optional run clock
 * must not drop its reply. Entering issuing_turn is a new start even when no
 * non-streaming phase was observed. Query keys bind separately to confirmed ids. */
function useStreamReadScope(
  cardId: string, streaming: boolean, phase: HarnessPhaseTag | null,
  runningTurnId: string | null, nextRead: () => number,
) {
  const [scope, setScope] = useState(() => ({ cardId, streaming, phase, turnId: runningTurnId, read: nextRead() }));
  const starts = scope.cardId !== cardId || (streaming && !scope.streaming)
    || (streaming && phase === 'issuing_turn' && scope.phase !== 'issuing_turn');
  const turnId = starts ? runningTurnId : runningTurnId ?? scope.turnId;
  if (starts || scope.streaming !== streaming || scope.phase !== phase || scope.turnId !== turnId) {
    setScope({ cardId, streaming, phase, turnId, read: starts ? nextRead() : scope.read });
  }
  return scope;
}
