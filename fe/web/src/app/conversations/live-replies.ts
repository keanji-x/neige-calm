import { useQuery, useQueryClient } from '@tanstack/react-query';
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
import { useState } from '../../ui/state/public.ts';

const NO_COPIES: readonly LiveReplyCopy[] = Object.freeze([]);

type HeldCopies = Readonly<{ cardId: string; copies: readonly LiveReplyCopy[] }>;

/**
 * The open conversation's streamed replies (#1923 S2), as agent turns for the transcript's tail.
 * Polls `GET …/harness/live` only while this card is mounted and a reply may stream; the query
 * layer's recovery gating applies as it does to every read. The copies are held per card, so a
 * switch never shows one card's text in another.
 */
export function useLiveReplies({ transport, unauthorized, cardId, enabled, phase, transcriptKey, items }: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  cardId: string;
  enabled: boolean;
  phase: HarnessPhaseTag | null;
  /** The key of the transcript query the rows come from: its results are counted and re-read here. */
  transcriptKey: readonly unknown[];
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
  /* How many transcript results this card has had. Counted rather than timed: a result in the
     same millisecond as a phase observation would otherwise be ambiguous. */
  const readTranscriptVersion = useCallback(
    () => client.getQueryState(transcriptKey)?.dataUpdateCount ?? 0, [client, transcriptKey],
  );
  const subscribeToQueries = useCallback((notify: () => void) => client.getQueryCache().subscribe(notify), [client]);
  const transcriptVersion = useSyncExternalStore(subscribeToQueries, readTranscriptVersion);

  /* A poll answered before this stretch of streaming began is an older turn's, kept in the cache. */
  const streamingSince = useRef<number | null>(null);
  useEffect(() => {
    streamingSince.current = streaming ? Date.now() : null;
  }, [streaming, cardId]);
  const pollAt = live.dataUpdatedAt;
  const reply = live.data;
  useEffect(() => {
    const since = streamingSince.current;
    if (reply === undefined || since === null || pollAt < since) return;
    apply({ kind: 'poll', reply, atMs: Date.now() });
  }, [apply, reply, pollAt]);

  /* Drawn from the copies the current transcript does not retire, so a stored row and its live
     copy never share a frame; the effect below forgets the retired ones. */
  const visible = useMemo(
    () => reconcileLiveReplies(copies, { kind: 'transcript', items, version: transcriptVersion }),
    [copies, items, transcriptVersion],
  );

  /* The transcript reads in flight are cancelled, synchronously, before the version is read, so
     every result counted after it comes from a fetch that started after the phase was seen. */
  useEffect(() => {
    if (phase === null || !awaitsSettling(visible, phase)) return;
    cancelThenInvalidate(client, transcriptKey);
    apply({ kind: 'phase', phase, transcriptVersion: readTranscriptVersion() });
  }, [apply, client, phase, readTranscriptVersion, transcriptKey, visible]);
  useEffect(() => {
    if (visible !== copies) apply({ kind: 'transcript', items, version: transcriptVersion });
  }, [apply, copies, items, transcriptVersion, visible]);

  return useMemo(() => liveReplyTurns(visible), [visible]);
}
