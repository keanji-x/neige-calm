import { useEffect, useMemo, useRef } from 'react';
import { useInfiniteQuery, useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { buildTranscript, FOLLOW_INSTALLATION_DEFAULT, harnessItemToTurns, isOptimisticConversationTurn, isConversationMessage, kernelQueuesInput, COMPACT_FAILURES, COMPACT_TEXT, MODEL_CHANGE_TEXT, PLANNER_MODEL_FAILURES, serverItemHighWater, transcriptRowToTurnOutcome, type PendingQueueEntry, type PlannerQueueWriteOutcome } from '../../../../core/domain/conversation.ts';
import { describeConversation, pendingConversationIds, withRememberedConversation, type ConversationFacts } from '../../../../core/domain/conversation-summary.ts';
import { useInitialInputObservation } from './initial-input.ts';
import { rememberQueuePlacements, presentPendingMessages, initialInputNeedsObservation, queueTombstoneHides as tombstoneHides, initialInputQueueView, withInitialInputFacts, reportedInputSource, type ReportedInput, type AcceptedInitialInput } from '../../../../core/domain/conversation-outbox.ts';
import { refusalText, writeFailureOf } from '../../../../core/domain/failure-class.ts';
import { PERMISSION_MODE_CHANGE_TEXT, PLANNER_PERMISSION_MODE_FAILURES } from '../../../../core/domain/planner-permission-mode.ts';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import type { ConversationStopFeedback } from '../../../../core/domain/conversation-stop.ts';
import { anchorRunningTurn, type RunningTurnAnchor } from '../../../../core/domain/conversation-meta.ts';
import { harnessItemsQueryOptions, modelCatalogQueryOptions, plannerRunQueryOptions, usePlannerMutations } from '../providers/queries.ts';
import { managedProvidersQueryOptions } from '../providers/managed-providers.ts';
import { useState } from '../../ui/state/public.ts';
import { useConversationCommands, useConversationSendOps, useConversationQueueWriteOut, useConversationRememberedTurns, useRememberedConversations } from './public.tsx';
import { NO_TURNS } from './registry-owner.ts';
import { useConversationStop } from './stop.ts';
import { useConversationRestart } from './restart.ts';
import { offersFreshSession } from '../../../../core/domain/conversation-restart.ts';
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
  const registry = useConversationCommands();
  const { remember, rememberedTurnsOf } = registry;
  const rememberedConversations = useRememberedConversations();
  const cardId = scope?.cardId ?? '';
  const trackId = scope?.id;
  const provider = scope?.provider;
  const trackTitle = scope?.title;
  const cardTitle = scope?.cardTitle;
  const scopeUpdatedAt = scope?.updatedAt;
  const scopeKind = scope?.kind ?? 'shared-spec';
  const capabilities = useQuery({ ...managedProvidersQueryOptions(transport, unauthorized), enabled: scope?.kind === 'track-assistant' });
  const historyReplacementSupported = scopeKind !== 'track-assistant' || (!capabilities.isError
    && capabilities.data?.find(answer => answer.availability.provider === scope?.provider)?.capabilities.history_replacement === true);
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
  const items = useMemo(() => (history.data?.pages ?? []).flat(), [history.data]);
  /* A remembered transcript is the reopen fallback while the first page is unknown;
       once any query data exists the server wins, even when empty. */
  /* Queued rows are supplied by the display projection, not twice from the server
     transcript. Raw `serverTurns` still sees every row for delivery reconciliation. */
  const rememberedSource = useConversationRememberedTurns(history.data === undefined && scope !== null ? cardId : null);
  const rememberedEntries = history.data === undefined ? rememberedSource : null;
  const queueWriteOut = useConversationQueueWriteOut(scope === null ? null : cardId);
  const serverTurns = useMemo(
    () => history.data === undefined
      ? (rememberedEntries ?? []).filter(isConversationMessage)
      : [...items].sort((left, right) => left.id - right.id).flatMap(harnessItemToTurns),
    [history.data, items, rememberedEntries],
  );
  const landedRun = runReads.startOf(run.data);
  const servedQueue = run.data?.pending ?? EMPTY_PENDING_QUEUE;
  const queueOps = useConversationSendOps(scope === null ? null : cardId);
  const accepted = queueOps.find((op): op is AcceptedInitialInput => op.phase === 'initial-accepted');
  const initialObservation = useInitialInputObservation(transport, unauthorized, cardId, accepted,
    scope !== null && routeIntent.visible !== false, history.isFetching || run.isFetching, registry.nextRead);
  const reportedInput = useMemo(() => {
    const facts: ReportedInput[] = servedQueue.map(entry => ({ kind: 'queue', entryId: entry.entry_id, rev: entry.rev, text: entry.text }));
    for (const turn of serverTurns) { const fact = reportedInputSource(turn); if (fact !== null) facts.push(fact); }
    const observation = initialObservation.data?.observation;
    if (observation?.kind === 'queued') facts.push({ kind: 'queue', entryId: observation.entryId, rev: observation.rev, text: observation.text });
    return facts;
  }, [servedQueue, serverTurns, initialObservation.data]);
  const acceptedInitial = useMemo(() => accepted === undefined ? undefined : withInitialInputFacts(accepted, reportedInput), [accepted, reportedInput]);
  const initialInput = useMemo(() => new Map(acceptedInitial !== undefined && initialInputNeedsObservation(acceptedInitial) && initialObservation.data !== undefined
    ? [[acceptedInitial.key, initialObservation.data]] : []), [acceptedInitial, initialObservation.data]);
  const initialInputError = acceptedInitial === undefined || !initialInputNeedsObservation(acceptedInitial) ? null : initialObservation.error !== null
    ? readErrorText(initialObservation.error, 'The first message’s current status could not be checked.')
    : null;
  const phase = run.data?.phase ?? null;
  const recovery = run.data?.recovery ?? null;
  const stalled = offersFreshSession(recovery);
  /* Anchored on the response's arrival (`dataUpdatedAt`); the fold keeps the earlier anchor for one turn. */
  const [runningAnchor, setRunningAnchor] = useState<RunningTurnAnchor | null>(null);
  const nextRunningAnchor = anchorRunningTurn(runningAnchor, run.data?.running_turn ?? null, run.dataUpdatedAt);
  if (nextRunningAnchor !== runningAnchor) setRunningAnchor(nextRunningAnchor);
  /* The run page owns pending input identity. Replace its persisted rows with
     the display projection below, keeping one bubble while the queue drains. */
  /* Tombstones for entries this client has had a `done` DELETE or steer for, until the
       cached page catches up. Keyed by card AND entry (entry ids are unique per card);
       the value is the rev written against, so an entry the kernel puts back one rev
       up is shown again (`tombstoneHides`). */
  const [forgotten, setForgotten] = useState<ReadonlyMap<string, number>>(() => new Map());
  // Let the initial-input owner account for its suppressed rows before ordinary local tombstones filter the queue.
  const initialQueue = useMemo(() => initialInputQueueView(servedQueue, acceptedInitial, initialObservation.data, landedRun),
    [servedQueue, acceptedInitial, initialObservation.data, landedRun]);
  const pendingQueue = useMemo(
    () => forgotten.size === 0 ? initialQueue.entries : initialQueue.entries.filter(entry =>
      !tombstoneHides(forgotten.get(forgottenKey(cardId, entry.entry_id)), entry.rev)),
    [initialQueue.entries, forgotten, cardId],
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
    () => new Map(pendingQueue.map((entry) => [entry.entry_id, entry.rev])), [pendingQueue],
  );
  const mutations = usePlannerMutations(transport, cardId, scope === null ? null : scope.id, unauthorized);
  /** What went wrong with an action of the card it names; another card's is never this one's. */
  const [compactPending, setCompactPending] = useState<ReadonlySet<string>>(() => new Set());
  const compactLeases = useRef(new Set<string>());
  const [actionError, setActionError] = useState<Readonly<{ cardId: string; message: string }> | null>(null);
  /** The card shown now; a model write's answer is that of the card it was made in. */
  const shownCardId = useRef(cardId);
  shownCardId.current = cardId;
  const serverEntries = useMemo(
    () => rememberedEntries !== null
      ? rememberedEntries.filter((entry) => !isOptimisticConversationTurn(entry))
      : buildTranscript(items.filter((row) => row.input_segments?.some((segment) => segment.origin?.queue_entry != null)
        || row.item_uuid === null || !pendingQueueIds.has(row.item_uuid)))
        .filter((entry) => entry.author !== 'you' || entry.queueEntryId == null || !pendingQueueIds.has(entry.queueEntryId)
          || (entry.inputSource?.entryRev != null && entry.inputSource.entryRev !== pendingQueueIds.get(entry.queueEntryId))),
    [items, pendingQueueIds, rememberedEntries],
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
  const restart = useConversationRestart({ cardId, recovery, restart: mutations.restart });
  const landedTranscript = transcriptReads.startOf(history.data);
  /* A refetch keeping `run.data` restamps it; this re-renders only because `run.dataUpdatedAt` is read above (#2068). */
  const landed = useMemo(() => ({ transcript: landedTranscript, run: landedRun }), [landedTranscript, landedRun]);
  const outbox = useConversationOutbox({
    cardId, transport, send: mutations.send, serverEntries, serverTurns, liveReplies, queuedEntries: pendingQueue,
    stalled, landed, initialInput, reportedInput, queuesInput: kernelQueuesInput(phase), highWater: serverItemHighWater(items),
    pressed: () => { setActionError(null); stop.clearFeedback(); restart.clearError(); },
  });
  const { view, accountQueueDisplay } = outbox;
  const [placementMemory, setPlacementMemory] = useState(() => ({ cardId, ops: queueOps, queue: pendingQueue,
    placements: rememberQueuePlacements(new Map(), queueOps, pendingQueue) }));
  let placements = placementMemory.placements;
  if (placementMemory.cardId !== cardId || placementMemory.ops !== queueOps || placementMemory.queue !== pendingQueue) {
    placements = rememberQueuePlacements(placementMemory.cardId === cardId ? placementMemory.placements : new Map(), queueOps, pendingQueue);
    setPlacementMemory({ cardId, ops: queueOps, queue: pendingQueue, placements });
  }
  const presentation = useMemo(() => presentPendingMessages(view.transcript, pendingQueue, view.shown,
    placements, view.failed, stalled, queueOps),
  [view.transcript, pendingQueue, view.shown, placements, view.failed, stalled, queueOps]);
  const queueDisplayAccounting = useMemo(() => accountQueueDisplay(presentation.transcript, initialQueue.suppressedRefs),
    [accountQueueDisplay, initialQueue.suppressedRefs, presentation.transcript]);
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
  const facts = useMemo<ConversationFacts | null>(() => trackId === undefined || provider === undefined ? null : {
    sourceCardId, cardId, trackId, trackTitle, provider, cardTitle: cardTitle ?? null, kind: scopeKind,
    state: scopeState, working, stalled, fallbackUpdatedAt: scopeUpdatedAt ?? 0,
  }, [sourceCardId, cardId, cardTitle, provider, scopeKind, scopeState, scopeUpdatedAt, trackId, trackTitle, working, stalled]);
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
    remember(durableConversation, serverEntries);
  }, [serverEntries, durableConversation, remember, rememberOn]);
  useEffect(() => {
    /* A `'rows'` route remembers every row it lists; `rememberOn` is compared against
         each row so another Track's row cannot write into this scope. */
    for (const row of serverRows) {
      if (row.trackId !== rememberOn) continue;
      /* The open row belongs to the effect above; writing the plain row over it here
               would undo it on every render. */
      if (row.id === conversation?.id || ownedCardIds?.includes(row.id)) continue;
      /* A row arrives with `turns` absent and a nullable `title` on the wire; carrying the
               remembered count keeps confirmed activity from being forgotten on refresh.
               The server owns its nullable title. */
      /* `updatedAt` never goes backwards: the listed row's time does not move when a
               turn is added, but the drawer knows and wrote it here. */
      const known = rememberedConversations.find((candidate) => candidate.id === row.id);
      remember(
        withRememberedConversation(row, known),
        rememberedTurnsOf(row.id),
      );
    }
  }, [conversation?.id, rememberedConversations, rememberedTurnsOf, remember, rememberOn, serverRows, ownedCardIds]);

  /* Server rows own names. The open projection adds local send feedback and activity. */
  const conversations = useMemo(() => conversation === null
    ? serverRows
    : serverRows.map((row) => row.id === conversation.id ? { ...conversation, archivedAt: row.archivedAt } : row), [conversation, serverRows]);
  const model = useMemo(() => run.data === undefined ? FOLLOW_INSTALLATION_DEFAULT
    : { model: run.data.model, reasoning_effort: run.data.reasoning_effort }, [run.data]);

  const observeQueueOutcome = <T extends PlannerQueueWriteOutcome>(outcome: T): T => {
    if (outcome.kind === 'stale') outbox.observeQueuedInput({ kind: 'queue', entryId: outcome.entryId, rev: outcome.rev, text: outcome.text });
    return outcome;
  };

  /* Held in the registry, so a strip remounted on the way back still sees its conversation's write out (#2068). */
  const deleteQueuedEntry = (entry: PendingQueueEntry) =>
    registry.holdQueueWrite(cardId, () => mutations.deleteQueued(entry.entry_id, entry.rev, removed => {
      // Import the real ACK before the mutation owner starts either reconciliation read.
      outbox.forgetQueuedEntry(removed);
      forgetQueuedEntry(entry);
    }).then(observeQueueOutcome));
  /* A successful steer delivers this entry; only its queue copy departs. */
  const steerQueuedEntry = phase === 'turn_running'
    ? (entry: PendingQueueEntry) => registry.holdQueueWrite(cardId,
      () => mutations.steerQueued(entry.entry_id, entry.rev, () => { forgetQueuedEntry(entry); }).then(observeQueueOutcome))
    : undefined;

  return {
    conversations,
    turns: conversation === null ? NO_TURNS : presentation.transcript,
    pending: pendingConversationIds(conversation, working, !stalled && view.sending),
    working,
    stalled,
    recovery,
    stopping,
    stopFeedback,
    sending: view.sending,
    sendBlocked: view.blocked,
    initialInputChecking: view.checkingInitialInput && initialInputError === null,
    initialInputError,
    retryInitialInput: () => { void initialObservation.refetch().catch(() => undefined); void history.refetch().catch(() => undefined); },
    awaitingConversationUpdate: view.awaitingReconciliation && history.error === null && run.error === null,
    pendingQueue: presentation.queue,
    pendingSends: presentation.sending,
    queueDisplayAccounting,
    pendingQueueOverflow,
    editQueuedEntry: (entry, text) => registry.holdQueueWrite(cardId, () => mutations.editQueued(entry, text, saved => {
      outbox.acknowledgeQueuedEdit(entry, saved);
    }).then(observeQueueOutcome)),
    deleteQueuedEntry,
    queueWriteOut,
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
    attachmentsSupported: (run.data?.attachments_supported ?? false) && (scopeKind !== 'track-assistant' || (!capabilities.isError
      && capabilities.data?.find(answer => answer.availability.provider === provider)?.capabilities.images === true)),
    historyReplacementSupported,
    manualCompactSupported: run.data?.manual_compact_supported === true,
    contextUsage: run.data?.context_usage_invalidated === true ? null : run.data?.token_usage ?? null,
    compactOutcome: run.data?.compact_outcome ?? null,
    contextUsageInvalidated: run.data?.context_usage_invalidated === true,
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
    retryHistory: () => { void history.refetch().catch(() => undefined); if (acceptedInitial !== undefined) void initialObservation.refetch().catch(() => undefined); },
    retryRun: () => { void run.refetch().catch(() => undefined); if (acceptedInitial !== undefined) void initialObservation.refetch().catch(() => undefined); },
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
    permissionMode: run.data?.permission_mode ?? null,
    setPermissionMode: (mode) => {
      const setFor = cardId;
      setActionError(null);
      void mutations.setPermissionMode(mode).catch((error: unknown) => {
        /* Dropped when it settles while another conversation is shown, as a model change's is (#2068). */
        if (shownCardId.current !== setFor) return;
        setActionError({ cardId: setFor, message: refusalText(writeFailureOf(error), PLANNER_PERMISSION_MODE_FAILURES,
          PERMISSION_MODE_CHANGE_TEXT.refused) ?? PERMISSION_MODE_CHANGE_TEXT.unknown });
      });
    },
  };
}
