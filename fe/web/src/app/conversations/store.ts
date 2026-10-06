import { useEffect, useMemo, useRef } from 'react';
import { useInfiniteQuery, useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { buildTranscript, FOLLOW_INSTALLATION_DEFAULT, harnessItemToTurns, isOptimisticConversationTurn, isConversationMessage, kernelQueuesInput, COMPACT_FAILURES, COMPACT_TEXT, MODEL_CHANGE_TEXT, PLANNER_MODEL_FAILURES, serverItemHighWater, transcriptRowToTurnOutcome, type PendingQueueEntry } from '../../../../core/domain/conversation.ts';
import { describeConversation, pendingConversationIds, withRememberedConversation, withRememberedTitle, type ConversationFacts } from '../../../../core/domain/conversation-summary.ts';
import { queueTombstoneHides as tombstoneHides } from '../../../../core/domain/conversation-outbox.ts';
import { refusalText, writeFailureOf } from '../../../../core/domain/failure-class.ts';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import type { ConversationStopFeedback } from '../../../../core/domain/conversation-stop.ts';
import { anchorRunningTurn, type RunningTurnAnchor } from '../../../../core/domain/conversation-meta.ts';
import { harnessItemsQueryOptions, modelCatalogQueryOptions, plannerRunQueryOptions, usePlannerMutations } from '../providers/queries.ts';
import { useState } from '../../ui/state/public.ts';
import { useConversationRegistry } from './public.tsx';
import { useConversationStop } from './stop.ts';
import { useConversationRestart } from './restart.ts';
import { useLiveReplies, useTranscriptReads } from './live-replies.ts';
import { useConversationOutbox, useRunReads } from './outbox.ts';
import type { ConversationRouteIntent, ConversationStore, PlannerConversationScope } from './contracts.ts';

/** Tombstone key: entry ids are unique per card, not globally. */
function forgottenKey(card: string, entryId: string): string {
  return `${card}\u0000${entryId}`;
}

/* A stable identity for "no queue page", so the memo below is not recomputed on
   every render by a fresh array literal. */
const EMPTY_PENDING_QUEUE: readonly PendingQueueEntry[] = Object.freeze([]);

export function useConversationStore(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
  scope: PlannerConversationScope | null,
  routeIntent: ConversationRouteIntent,
): ConversationStore {
  const registry = useConversationRegistry();
  const cardId = scope?.cardId ?? '';
  const trackId = scope?.id;
  const trackTitle = scope?.title;
  const cardTitle = scope?.cardTitle;
  const scopeUpdatedAt = scope?.updatedAt;
  const scopeKind = scope?.kind ?? 'shared-spec';
  const scopeState = scope?.state ?? null;
  const serverRows = routeIntent.rows;
  const rememberOn = routeIntent.rememberOn;
  const ownedCardIds = routeIntent.ownedCardIds;
  const sourceCardId = serverRows.find((row) => row.id === cardId)?.sourceCardId;
  /* Held across renders: the live replies read and re-read this query by its key, and number its reads. */
  const transcriptReads = useTranscriptReads(registry.nextRead);
  const transcriptQuery = useMemo(
    () => transcriptReads.track(harnessItemsQueryOptions(transport, cardId, unauthorized)),
    [transcriptReads, transport, cardId, unauthorized],
  );
  const history = useInfiniteQuery({ ...transcriptQuery, enabled: scope !== null });
  /* Numbered as the transcript's are: a send answered after an unknown attempt waits for reads started after it. */
  const runReads = useRunReads(registry.nextRead);
  const runQuery = useMemo(
    () => runReads.track(plannerRunQueryOptions(transport, cardId, unauthorized)), [runReads, transport, cardId, unauthorized],
  );
  const run = useQuery({ ...runQuery, enabled: scope !== null });
  /* The catalog rides alongside the run query: the trigger has to render the chosen
       model's name, and `planner-run` gives only its slug. */
  const modelCatalog = useQuery({
    ...modelCatalogQueryOptions(transport, { kind: 'card', cardId }, unauthorized), enabled: scope !== null,
  });
  const phase = run.data?.phase ?? null;
  const stalled = phase === 'wedged';
  /* Anchored on the response's arrival (`dataUpdatedAt`); the fold keeps the earlier anchor for one turn. */
  const [runningAnchor, setRunningAnchor] = useState<RunningTurnAnchor | null>(null);
  const nextRunningAnchor = anchorRunningTurn(runningAnchor, run.data?.running_turn ?? null, run.dataUpdatedAt);
  if (nextRunningAnchor !== runningAnchor) setRunningAnchor(nextRunningAnchor);
  /* `pendingQueueIds` is the visibility judgement for the echoes below: an entry the
       queue region is drawing must not also be drawn in the transcript. */
  /* Tombstones for entries this client has had a `done` DELETE or steer for, until the
       cached page catches up. Keyed by card AND entry (entry ids are unique per card);
       the value is the rev written against, so an entry the kernel puts back one rev
       up is shown again (`tombstoneHides`). */
  const [forgotten, setForgotten] = useState<ReadonlyMap<string, number>>(() => new Map());
  const servedQueue = run.data?.pending ?? EMPTY_PENDING_QUEUE;
  const pendingQueue = useMemo(
    () => (forgotten.size === 0
      ? servedQueue
      : servedQueue.filter((entry) => !tombstoneHides(forgotten.get(forgottenKey(cardId, entry.entry_id)), entry.rev))),
    [servedQueue, forgotten, cardId],
  );
  useEffect(() => {
    if (forgotten.size === 0) return;
    /* Retired when the owning page says the entry is gone or lists it at a higher rev;
           keys for other cards are left alone. */
    const servedRev = new Map(servedQueue.map((entry) => [forgottenKey(cardId, entry.entry_id), entry.rev]));
    const mine = (key: string) => key.startsWith(`${cardId}\u0000`);
    const retired = (key: string, wroteAt: number): boolean => {
      const rev = servedRev.get(key);
      return rev === undefined || !tombstoneHides(wroteAt, rev);
    };
    /* Only when there is something to drop — an unconditional `setForgotten`
       here re-renders forever. */
    if (![...forgotten].some(([key, wroteAt]) => mine(key) && retired(key, wroteAt))) return;
    setForgotten((current) => new Map(
      [...current].filter(([key, wroteAt]) => !mine(key) || !retired(key, wroteAt)),
    ));
  }, [servedQueue, forgotten, cardId]);
  const forgetQueuedEntry = (entry: PendingQueueEntry): void => {
    setForgotten((current) => new Map([...current, [forgottenKey(cardId, entry.entry_id), entry.rev]]));
  };
  const pendingQueueOverflow = run.data?.pending_overflow ?? 0;
  const pendingQueueIds = useMemo(
    () => new Set(pendingQueue.map((entry) => entry.entry_id)), [pendingQueue],
  );
  const mutations = usePlannerMutations(transport, cardId, unauthorized);
  /** What went wrong with an action of the card it names; another card's is never this one's. */
  const [compactPending, setCompactPending] = useState<ReadonlySet<string>>(() => new Set());
  const compactLeases = useRef(new Set<string>());
  const [actionError, setActionError] = useState<Readonly<{ cardId: string; message: string }> | null>(null);
  /** The card shown now; a model write's answer is that of the card it was made in. */
  const shownCardId = useRef(cardId);
  shownCardId.current = cardId;
  const items = useMemo(() => (history.data?.pages ?? []).flat(), [history.data]);
  /* A remembered transcript is the reopen fallback while the first page is unknown;
       once any query data exists the server wins, even when empty. */
  /* An entry the queue region is currently listing is drawn there and nowhere else;
       only the rendered transcript is filtered, `serverTurns` still sees the row. */
  const serverEntries = useMemo(
    () => history.data === undefined
      ? registry.turnsOf(cardId).filter((entry) => !isOptimisticConversationTurn(entry))
      : buildTranscript(items.filter((row) => row.item_uuid === null || !pendingQueueIds.has(row.item_uuid))),
    [cardId, history.data, items, pendingQueueIds, registry],
  );
  const serverTurns = useMemo(
    () => history.data === undefined
      ? serverEntries.filter(isConversationMessage)
      : [...items].sort((left, right) => left.id - right.id).flatMap(harnessItemToTurns),
    [history.data, items, serverEntries],
  );
  useEffect(() => { setActionError(null); }, [cardId]);
  /* The running turn's streamed replies: drawn at the tail, never remembered, never counted. */
  const liveReplies = useLiveReplies({
    transport, unauthorized, cardId, enabled: scope !== null, phase,
    runningTurnId: run.data?.running_turn?.turn_id ?? null, nextRead: registry.nextRead,
    transcriptKey: transcriptQuery.queryKey, transcriptReads, items,
  });
  const working = phase === 'issuing_turn' || phase === 'compacting' || phase === 'turn_running';
  const stop = useConversationStop({
    cardId, canStop: working && !stalled,
    responseEnded: phase === 'idle' || phase === 'turn_completed',
    historyKnown: history.data !== undefined,
    newestRowId: items.reduce((latest, row) => Math.max(latest, row.id), 0),
    completedRowId: items.reduce<number | null>((latest, row) => transcriptRowToTurnOutcome(row) === null
      ? latest : Math.max(latest ?? 0, row.id), null),
    requestStop: mutations.interrupt,
  });
  const restart = useConversationRestart({ cardId, stalled, restart: mutations.restart });
  const landedTranscript = transcriptReads.startOf(history.data);
  /* A refetch keeping `run.data` restamps it; this re-renders only because `run.dataUpdatedAt` is read above (#2068). */
  const landedRun = runReads.startOf(run.data);
  const landed = useMemo(() => ({ transcript: landedTranscript, run: landedRun }), [landedTranscript, landedRun]);
  const outbox = useConversationOutbox({
    cardId, transport, send: mutations.send, serverEntries, serverTurns, liveReplies, queuedEntryIds: pendingQueueIds,
    stalled, landed, queuesInput: kernelQueuesInput(phase), highWater: serverItemHighWater(items),
    pressed: () => { setActionError(null); stop.clearFeedback(); restart.clearError(); },
  });
  const { view } = outbox;
  /* What the reader is looking at, and what the tab may remember: a message is the conversation's only once the
     server has answered it, and only until a read shows it in its place. */
  const turns = useMemo(
    () => [...serverTurns, ...view.shown].sort((left, right) => left.atMs - right.atMs), [serverTurns, view.shown],
  );
  const confirmedTurns = useMemo(
    () => [...serverTurns, ...view.confirmed].sort((left, right) => left.atMs - right.atMs), [serverTurns, view.confirmed],
  );
  const stopping = !stalled && (phase === 'issuing_interrupt' || (working && stop.pending));
  const stopFeedback: ConversationStopFeedback | null = stalled ? null
    : phase === 'issuing_interrupt' ? { kind: 'stopping' }
    : stop.feedback?.kind === 'requesting' && !working ? null : stop.feedback;
  const facts = useMemo<ConversationFacts | null>(() => trackId === undefined ? null : {
    sourceCardId, cardId, trackId, trackTitle, cardTitle: cardTitle ?? null, kind: scopeKind,
    state: scopeState, working, stalled, fallbackUpdatedAt: scopeUpdatedAt ?? 0,
  }, [sourceCardId, cardId, cardTitle, scopeKind, scopeState, scopeUpdatedAt, trackId, trackTitle, working, stalled]);
  /** What the reader is looking at: every turn, echoes included. */
  const conversation = useMemo(
    () => facts === null ? null : describeConversation(facts, turns), [facts, turns],
  );
  /**
   * What the tab will still believe once the drawer is gone: confirmed turns only.
   * The registry is a memory kept for the life of the tab, so a fact that is not
   * yet a fact may never enter it.
   */
  const durableConversation = useMemo(
    () => facts === null ? null : describeConversation(facts, confirmedTurns),
    [confirmedTurns, facts],
  );
  useEffect(() => {
    if (durableConversation === null) return;
    /* Server rows enter the registry only under the Track that supplied them. */
    if (durableConversation.trackId !== rememberOn) return;
    /* What the reads showed; the registry adds this conversation's confirmed sends back from its outbox. */
    registry.remember(durableConversation, serverEntries);
  }, [serverEntries, durableConversation, registry, rememberOn]);
  useEffect(() => {
    /* A `'rows'` route remembers every row it lists; `rememberOn` is compared against
         each row so another Track's row cannot write into this scope. */
    for (const row of serverRows) {
      if (row.trackId !== rememberOn) continue;
      /* The open row belongs to the effect above; writing the plain row over it here
               would undo it on every render. */
      if (row.id === conversation?.id || ownedCardIds?.includes(row.id)) continue;
      /* A row arrives with `turns` absent and `title` null on the wire; carrying the
               remembered values keeps a confirmed count and derived name from being
               forgotten on refresh. A title the server does send wins. */
      /* `updatedAt` never goes backwards: the listed row's time does not move when a
               turn is added, but the drawer knows and wrote it here. */
      const known = registry.conversations.find((candidate) => candidate.id === row.id);
      registry.remember(
        withRememberedConversation(row, known),
        registry.turnsOf(row.id),
      );
    }
  }, [conversation?.id, registry, rememberOn, serverRows, ownedCardIds]);

  const listedConversations = useMemo(
    () => serverRows.map((row) => withRememberedTitle(
      row, registry.conversations.find((candidate) => candidate.id === row.id),
    )),
    [registry.conversations, serverRows],
  );
  /* The open row is replaced in place by the live one: same id, plus the turns and
       name only the transcript can supply. */
  const conversations = useMemo(() => conversation === null
    ? listedConversations
    : listedConversations.map((row) => row.id === conversation.id ? conversation : row), [conversation, listedConversations]);
  const model = useMemo(() => run.data === undefined ? FOLLOW_INSTALLATION_DEFAULT
    : { model: run.data.model, reasoning_effort: run.data.reasoning_effort }, [run.data]);

  /* Held in the registry, so a strip remounted on the way back still sees its conversation's write out (#2068). */
  const deleteQueuedEntry = (entry: PendingQueueEntry) =>
    registry.holdQueueWrite(cardId, () => mutations.deleteQueued(entry.entry_id, entry.rev)).then((outcome) => {
      /* `gone` is not a retirement: the entry drained and its transcript row is on its way. */
      if (outcome.kind === 'done') {
        outbox.forgetQueuedEntry(entry.entry_id);
        forgetQueuedEntry(entry);
      }
      return outcome;
    });
  /* On a steer's `done` the entry is forgotten but its send is NOT: a steer delivers the sentence, and the
       kernel's transcript row shows it. */
  const steerQueuedEntry = phase === 'turn_running'
    ? (entry: PendingQueueEntry) =>
      registry.holdQueueWrite(cardId, () => mutations.steerQueued(entry.entry_id, entry.rev)).then((outcome) => {
        if (outcome.kind === 'done') forgetQueuedEntry(entry);
        return outcome;
      })
    : undefined;

  return {
    conversations,
    turnsOf: (conversationId) => conversation?.id === conversationId ? view.transcript : registry.turnsOf(conversationId),
    pending: pendingConversationIds(conversation, working, !stalled && view.sending),
    working,
    stalled,
    stopping,
    stopFeedback,
    sending: view.sending,
    sendBlocked: view.blocked,
    pendingQueue,
    pendingQueueOverflow,
    deleteQueuedEntry,
    queueWriteOut: registry.queueWriteOutOf(cardId),
    steerQueuedEntry,
    historyReady: history.data !== undefined,
    historyLoading: history.isFetching,
    hasEarlier: history.hasNextPage,
    loadingEarlier: history.isFetchingNextPage,
    historyError: history.error === null ? null : readErrorText(history.error, 'The conversation history could not be loaded.'),
    runError: run.error === null ? null : readErrorText(run.error, 'The conversation’s status could not be loaded.'),
    runLoading: run.isFetching,
    runReady: run.data !== undefined,
    actionError: actionError?.cardId === cardId ? actionError.message : null,
    failedSend: view.failed,
    retrySend: outbox.retrySend,
    discardFailedSend: outbox.discardFailedSend,
    dismissFailedSend: outbox.dismissFailedSend,
    send: outbox.send,
    attachmentsSupported: run.data?.attachments_supported ?? false,
    contextUsage: run.data?.token_usage ?? null,
    runningAnchor: nextRunningAnchor,
    uploadAttachment: mutations.uploadAttachment,
    compacting: compactPending.has(cardId) || phase === 'compacting',
    compact: () => {
      if (compactLeases.current.has(cardId) || cardId === '') return;
      const compactFor = cardId;
      compactLeases.current.add(compactFor);
      setCompactPending((current) => new Set([...current, compactFor]));
      setActionError(null);
      void mutations.compact().catch((error: unknown) => {
        if (shownCardId.current === compactFor) setActionError({ cardId: compactFor,
          message: refusalText(writeFailureOf(error), COMPACT_FAILURES, COMPACT_TEXT.refused) ?? COMPACT_TEXT.unknown });
      }).finally(() => {
        compactLeases.current.delete(compactFor);
        setCompactPending((current) => {
          const next = new Set(current);
          next.delete(compactFor);
          return next;
        });
      });
    },
    interrupt: stop.interrupt,
    restart: { strip: restart.strip, pending: restart.pending, start: restart.start },
    retryHistory: () => { void history.refetch().catch(() => undefined); },
    retryRun: () => { void run.refetch().catch(() => undefined); },
    loadEarlier: () => { void history.fetchNextPage().catch(() => undefined); },
    blockedReason: run.data?.blocked_reason ?? null,
    /* Before the first answer the conversation is following the default, which is
           what a card with no selection really does. */
    model,
    modelCatalog: modelCatalog.data ?? null,
    setModel: (selection) => {
      const setFor = cardId;
      setActionError(null);
      /* Dropped when it settles while another conversation is shown: kept, it would show for one commit on the way back (#2068). */
      const fail = (message: string) => { if (shownCardId.current === setFor) setActionError({ cardId: setFor, message }); };
      void mutations.setModel(selection)
        .then((result) => {
          /* Both flags are reported: the write succeeded, but the value stored is not
                       quite the value asked for. */
          if (result.effort_adjusted) {
            fail(`That reasoning effort is not available on this model; it now uses ${result.reasoning_effort ?? 'the default'}.`);
          } else if (result.unknown_model) {
            fail('codex does not list that model for this account. It is saved; turns may fail.');
          }
        })
        .catch((error: unknown) => {
          fail(refusalText(writeFailureOf(error), PLANNER_MODEL_FAILURES, MODEL_CHANGE_TEXT.refused) ?? MODEL_CHANGE_TEXT.unknown);
        });
    },
  };
}
