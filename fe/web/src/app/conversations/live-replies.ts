import { useQuery, useQueryClient, type InfiniteData } from '@tanstack/react-query';
import { useCallback, useEffect, useMemo, useRef, useSyncExternalStore } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { HarnessPhaseTag } from '../../../../core/api/generated/wire.ts';
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

type HeldCopies = Readonly<{ cardId: string; copies: readonly LiveReplyCopy[] }>;

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
export function useLiveReplies({ transport, unauthorized, cardId, enabled, phase, transcriptKey, transcriptReads, items }: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  cardId: string;
  enabled: boolean;
  phase: HarnessPhaseTag | null;
  /** The key of the transcript query the rows come from: its results are read and re-read here. */
  transcriptKey: readonly unknown[];
  /** The numbering that query's reads were tracked with. */
  transcriptReads: TranscriptReads;
  /** Every loaded transcript row. */
  items: readonly LiveReplyTranscriptRow[];
}): readonly ConversationTurn[] {
  const client = useQueryClient();
  const streaming = enabled && replyMayStream(phase);
  const live = useQuery({
    ...harnessLiveQueryOptions(transport, cardId, unauthorized),
    enabled: streaming,
    refetchInterval: LIVE_REPLY_POLL_MS,
  });
  const [held, setHeld] = useState<HeldCopies>(() => ({ cardId, copies: NO_COPIES }));
  const copies = held.cardId === cardId ? held.copies : NO_COPIES;
  /* Leaving a card forgets its copies, so returning to it never brings back text from before. */
  useEffect(() => {
    setHeld((current) => current.cardId === cardId ? current : { cardId, copies: NO_COPIES });
  }, [cardId]);
  const apply = useCallback((observation: LiveReplyObservation) => {
    setHeld((current) => {
      const base = current.cardId === cardId ? current.copies : NO_COPIES;
      const next = reconcileLiveReplies(base, observation);
      return next === base && current.cardId === cardId ? current : { cardId, copies: next };
    });
  }, [cardId]);
  /* Which read the stored transcript's newest page came from. Numbered rather than timed: a read
     started in the same millisecond as a phase observation would otherwise be ambiguous. */
  const readTranscriptStart = useCallback(
    () => transcriptReads.startOf(client.getQueryData<TranscriptResult>(transcriptKey)),
    [client, transcriptKey, transcriptReads],
  );
  const subscribeToQueries = useCallback((notify: () => void) => client.getQueryCache().subscribe(notify), [client]);
  const transcriptStart = useSyncExternalStore(subscribeToQueries, readTranscriptStart);

  /* A poll answered before this stretch of streaming began is an older turn's, kept in the cache. */
  const streamingSince = useRef<number | null>(null);
  useEffect(() => {
    streamingSince.current = streaming ? Date.now() : null;
  }, [streaming, cardId]);
  const pollAt = live.dataUpdatedAt;
  const reply = live.data;
  /* KNOWN GAP: a poll still in flight when streaming stops can land after the next stretch began (one turn ends and the next starts within a round trip) and re-add the old turn's copy briefly; (a) hides it at once if stored, (c) removes it within one poll otherwise. */
  useEffect(() => {
    const since = streamingSince.current;
    if (reply === undefined || since === null || pollAt < since) return;
    apply({ kind: 'poll', reply, atMs: Date.now() });
  }, [apply, reply, pollAt]);

  /* Drawn from the copies the current transcript does not retire, so a stored row and its live
     copy never share a frame; the effect below forgets the retired ones. */
  const visible = useMemo(
    () => reconcileLiveReplies(copies, { kind: 'transcript', items, readStart: transcriptStart }),
    [copies, items, transcriptStart],
  );

  /* Only a read started after this point stands for the phase, however late an earlier one lands.
     The transcript is re-read here so that one always starts; the cancel comes first because the
     query layer would hand back an initial read still in flight in place of a new one. */
  const rereadAfter = useRef<number | null>(null);
  const reread = useCallback(() => {
    rereadAfter.current = transcriptReads.latest();
    cancelThenInvalidate(client, transcriptKey);
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
