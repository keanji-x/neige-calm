import { admitTransport } from '../providers/recovery-mutation.ts';
// Code-based TanStack Router setup, built inside a factory so a test can inject the
// transport and QueryClient; also the composition point for route-owned surfaces.

import {
  createRootRoute, createRoute, createRouter, type AnyRoute,
} from '@tanstack/react-router';
import { useCallback, useEffect, useMemo, useRef } from 'react';
import { TrackViewProvider, useTrackViewState } from './track-view-state.tsx';
import { HStack } from '@astryxdesign/core/HStack';
import { onlineManager, useInfiniteQuery, useQuery, type QueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { PlannerAttachment } from '../../../../core/api/generated/wire.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  ATTACHED_WORKSPACE_REASON, PlannerAttachButton, PlannerAttachmentDrawer,
  type UploadAttachment, usePlannerAttachments,
} from '../../features/planner/attachments.tsx';
import { hasUnseenMatchingConversationMessage, failedConversationDelivery } from '../../../../core/domain/conversation-delivery.ts';
import {
  cardGoalTitle, liveTableOverlayPayload, toTrack, trackActivityFrom, trackDisplayTitle,
  type Track, type TrackActivity, type TrackDetailWire,
} from '../../../../core/domain/track.ts';
import { cardActivityOf, foldAttentionByCard, type CardActivity } from '../../../../core/domain/activity.ts';
import type {
  BoardHostItem, CardAddMenuEntry, CardHost, CardRegistry,
} from '../../systems/cards/public.js';
import {
  cardAddMenuEntries, isAssistantHarnessPayload, isPlannerHarnessPayload, partitionTrackCards,
} from '../../systems/cards/public.js';
import { mintIdempotencyKey } from './idempotency-key.ts';
import footerStyles from './composer-footer.module.css';
import { TodayPage } from '../../features/today/public.tsx';
import { nameTodaySummaryConversation } from '../../../../core/domain/today.ts';
import { TrackRow } from '../../features/track/row/public.tsx';
import { TrackPage, type TrackInputNotification } from '../../features/track/page/public.tsx';
import { CardGridOverlay, TrackStage } from '../../features/track/grid/public.tsx';
import { AddCardMenu, NewCardForm, type NewCardValues } from '../../features/track/new-card/public.tsx';
import { ChatList } from '../../features/chat/list/public.tsx';
import {
  ChatComposer, ChatFooterError, ChatFooterNotice, ChatFooterRemedy, ChatThread,
} from '../../features/chat/thread/public.tsx';
import { ModelPill } from '../../features/chat/thread/model-pill.tsx';
import { ContextRing } from '../../features/chat/thread/context-ring.tsx';
import { ReportBacklinks } from '../../features/report/backlinks/public.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { useIndependentTaskLaunch } from './independent-task.tsx';
import { useTaskArtifactFiles } from './task-artifact-files.tsx';
import { TaskRecovery, useCurrentTaskRows } from './task-recovery.tsx';
import { useReportSeriesResolver } from './report-series.ts';
import { ReportSourceDrawer } from './report-source.tsx';
import { ReportEmpty } from '../../features/report/empty/public.tsx';
import { ReportFileViewer } from '../../features/report/file-viewer/public.tsx';
import { ReportOutline } from '../../features/report/outline/public.tsx';
import { RecentFiles } from '../../features/report/recent-files/public.tsx';
import { revealReportAnchor } from '../../features/report/anchor/public.ts';
import {
  backlinkCountsByBlock, deriveReportOutline, deriveReportTasks, readTrackReport, type ReportLinkTarget,
} from '../../../../core/domain/report.ts';
import {
  parseWorkspaceRelativeFilePath, type ReportFileLinkTarget,
} from '../../../../core/domain/report-file.ts';
import type { ReportSourceLinkTarget } from '../../../../core/domain/report-source.ts';
import {
  buildTranscript, conversationName, conversationNameFrom, CONVERSATION_STATE_SOURCE,
  conversationCreateFailure, CONVERSATION_TEXT_MAX, harnessItemToTurns, isOptimisticConversationTurn,
  isConversationMessage, isSendRefusalCode, kernelQueuesInput,
  mergeTranscript, reconcileOptimisticConversationTurns, serverItemHighWater,
  trackConversationCardId,
  FOLLOW_INSTALLATION_DEFAULT,
  type Conversation, type ConversationKind, type ConversationMessage, type ConversationState,
  type ModelCatalog, type ModelSelection,
  type OptimisticConversationTurn, type PendingQueueEntry, type PlannerRunTokenUsage,
  type PlannerQueueWriteOutcome, type SendOutcome, type TranscriptEntry,
} from '../../../../core/domain/conversation.ts';
import { ConfirmDialog, Dialog } from '../../ui/dialog/public.tsx';
import { createDirectoryLister, createTrackWorkspaceFilesPort } from '../providers/directory.ts';
import { DELETE_CARD_COPY, DELETE_TRACK_COPY, RESET_TODAY_REPORT_COPY } from '../../ui/confirm-dialog/copy.ts';
import { OperationFeedback, useDeleteConfirm, useOperationFeedback } from '../../ui/operation-feedback/public.tsx';
import { Drawer } from '../../ui/drawer/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { PanelAction, PanelEmpty } from '../../ui/panel-card/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  ApiError, OfflineSubmissionError, apiFailureCodeOf, harnessItemsQueryOptions,
  modelCatalogQueryOptions, serverVersionOperation, runOperation,
  prefetchAreaList, plannerRunQueryOptions, todayLaunchpadQueryOptions,
  usePlannerMutations, useTodayLaunchpadEnsureMutation, useTodayReportResetMutation,
  useTrackConversationMutations, useTrackMutations, useTrackRecipeMutations, useTrackRecipes,
  useWorkspace,
  trackBacklinksQueryOptions, trackConversationsQueryOptions, trackDetailQueryOptions,
  trackOverlaysQueryOptions, trackTaskVerdictsQueryOptions,
} from '../providers/queries.ts';
import { NewTrackRoute } from './new-track-route.tsx';
import { NewTrackDraftProvider } from './new-track-drafts.tsx';
import {
  RecipesPage, type RecipeDraft, type RecipeWriteOutcome,
} from '../../features/report/recipe/public.tsx';
import { useTheme } from '../theme/public.tsx';
import { createUiPreferences, UiPreferencesProvider, useConversationViewTarget, useUiPreferences, useReadReceipt, type UiPreferences } from '../providers/ui-preferences.tsx';
import { TrackSelector } from '../shell/track-selector.tsx';
import { AppShell, useOpenMobileSection, useMobileHeaderActionsHost, useMobileHeaderTitleHost, useMobileTrackChoices } from '../shell/public.tsx';
import {
  ConversationProvider, useConversationRegistry,
  type ConversationDraft, type ConversationDraftId, type FailedConversationSend,
} from '../conversations/public.tsx';
import {
  renderedMobilePanel,
  useGo, useGoSameTrack, useRouteCardId, useRouteFilePath, useRouteFrom, useRouteHash,
  useRoutePanel, useRouteParam, useTrackFileNavigation,
  usePlannerOpenIntent, useTrackPanelNavigation, validateTrackSearch, type TrackSearch,
} from './navigation.ts';
import {
  createRecentFileHistory, type RecentFileHistory,
} from '../providers/recent-files.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { PendingRoute } from './pending-route.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { PendingQueue } from '../../features/planner/public.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';

export const APP_BASEPATH = '/next';

type ConversationStore = Readonly<{
  conversations: readonly Conversation[];
  /** Messages *and* the actions between them, in the order they happened. */
  turnsOf: (conversationId: string) => readonly TranscriptEntry[];
  pending: ReadonlySet<string>;
  working: boolean;
  stalled: boolean;
  stopping: boolean;
  sending: boolean;
  sendBlocked: boolean;
  /** The addressable page of the harness pending queue. */
  pendingQueue: readonly PendingQueueEntry[];
  /** Queued messages that exist but carry no id to address them by. */
  pendingQueueOverflow: number;
  deleteQueuedEntry: (entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>;
  /** Hand a queued entry to the running turn; `undefined` outside `turn_running`, and that is the whole gate. */
  steerQueuedEntry: ((entry: PendingQueueEntry) => Promise<PlannerQueueWriteOutcome>) | undefined;
  historyReady: boolean;
  historyLoading: boolean;
  hasEarlier: boolean;
  loadingEarlier: boolean;
  historyError: string | null;
  actionError: string | null;
  failedSend: FailedConversationSend | null;
  matchingSendMessage: boolean;
  retrySend: (echoId: string) => void;
  /** What became of the send. `attachments` are ids already uploaded; naming one here is what makes it permanent. */
  send: (conversationId: string, text: string, attachments?: readonly PlannerAttachment[]) => Promise<SendOutcome>;
  /** Whether this card's track can take image attachments at all. */
  attachmentsSupported: boolean;
  /** How full this conversation's context is; `null` when the harness has never said. */
  contextUsage: PlannerRunTokenUsage | null;
  uploadAttachment: UploadAttachment;
  interrupt: () => void;
  retryHistory: () => void;
  loadEarlier: () => void;
  /** Why the queue is not draining, when the reader has to act; a standing condition of the conversation, unlike `actionError`. */
  blockedReason: string | null;
  /** What this conversation's turns run with. */
  model: ModelSelection;
  /** What may be chosen, or `null` until the catalog has answered once. */
  modelCatalog: ModelCatalog | null;
  /** Store a whole new selection. Failures land in `actionError`, like a send's. */
  setModel: (selection: ModelSelection) => void;
}>;

/** A server-backed list and the real Track whose rows may enter the tab registry. */
type ConversationRouteIntent = Readonly<{
  rows: readonly Conversation[];
  rememberOn: string;
}>;

export function pendingConversationIds(
  conversation: Conversation | null, working: boolean, sending: boolean,
): ReadonlySet<string> {
  return (working || sending) && conversation !== null ? new Set([conversation.id]) : new Set();
}

/** Tombstone key: entry ids are unique per card, not globally. */
function forgottenKey(card: string, entryId: string): string {
  return `${card}\u0000${entryId}`;
}

/**
 * Whether a tombstone written at `wroteAt` still hides an entry listed at `rev`:
 * a higher rev is the kernel's word that it changed the entry after the client last saw it.
 */
function tombstoneHides(wroteAt: number | undefined, rev: number): boolean {
  return wroteAt !== undefined && rev <= wroteAt;
}

/* A stable identity for "no queue page", so the memo below is not recomputed on
   every render by a fresh array literal. */
const EMPTY_PENDING_QUEUE: readonly PendingQueueEntry[] = Object.freeze([]);

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message !== '' ? error.message : fallback;
}

/** Everything about the open conversation that does *not* come from its turns. */
type ConversationFacts = Readonly<{
  cardId: string;
  trackId: string;
  trackTitle: string | undefined;
  cardTitle: string | null;
  kind: ConversationKind;
  state: ConversationState | null;
  working: boolean;
  stalled: boolean;
  /** The row's own time, used when no turn has supplied a later one. */
  fallbackUpdatedAt: number;
}>;

/** The conversation row these turns describe; a function of the turns because "shown" and "happened" are not the same claim. */
function describeConversation(
  facts: ConversationFacts, turns: readonly ConversationMessage[],
): Conversation {
  return {
    id: facts.cardId, trackId: facts.trackId,
    /* Absent, not `''`: list rows do not repeat the surrounding Track title.
       `ChatList` renders the difference; `''` would render a blank. */
    ...(facts.trackTitle === undefined ? {} : { trackTitle: facts.trackTitle }),
    title: facts.cardTitle
      ?? conversationNameFrom(turns.find((turn) => turn.author === 'you')?.text ?? ''),
    kind: facts.kind,
    /* The server's state is the server's to report (`run_status_for` writes `turn_pending`,
           never `running`); the local phase wins only while a turn is in flight.
           `CONVERSATION_STATE_SOURCE` is a total table so a new kind cannot silently fall into `else`. */
    state: facts.stalled ? 'failed' : CONVERSATION_STATE_SOURCE[facts.kind] === 'server'
      ? (facts.working ? 'turn_pending' : facts.state)
      : (facts.working ? 'running' : 'idle'),
    updatedAt: turns.at(-1)?.atMs ?? facts.fallbackUpdatedAt,
    turns: turns.length,
  };
}

/** Project confirmed facts this tab learned back onto a server summary; server facts win when present, and time never moves backwards. */
function withRememberedConversation(
  row: Conversation, remembered: Conversation | undefined,
): Conversation {
  return {
    ...row,
    ...(remembered?.turns === undefined ? {} : { turns: remembered.turns }),
    ...(row.title === null && remembered?.title != null ? { title: remembered.title } : {}),
    updatedAt: Math.max(row.updatedAt, remembered?.updatedAt ?? 0),
  };
}

/** A derived first-message title is stable and may be shown after close; counts and activity time are snapshots only the open row may claim. */
function withRememberedTitle(
  row: Conversation, remembered: Conversation | undefined,
): Conversation {
  return {
    ...row,
    ...(row.title === null && remembered?.title != null ? { title: remembered.title } : {}),
  };
}

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
  const history = useInfiniteQuery({
    ...harnessItemsQueryOptions(transport, cardId, unauthorized), enabled: scope !== null,
  });
  const run = useQuery({ ...plannerRunQueryOptions(transport, cardId, unauthorized), enabled: scope !== null });
  /* The catalog rides alongside the run query: the trigger has to render the chosen
       model's name, and `planner-run` gives only its slug. */
  const modelCatalog = useQuery({
    ...modelCatalogQueryOptions(transport, cardId, unauthorized), enabled: scope !== null,
  });
  const phase = run.data?.phase ?? null;
  const stalled = phase === 'wedged';
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
  const [echoes, setEchoes] = useState<readonly OptimisticConversationTurn[]>([]);
  /** The echo whose `POST /planner/input` is unanswered. One id, not a set: a second unanswered echo would make `confirmedEchoes` report the first as confirmed. */
  const [unconfirmedEchoId, setUnconfirmedEchoId] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [interruptPending, setInterruptPending] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const sendingRef = useRef(false);
  /** The send whose settling may still speak for this store; a request that is no longer this one says nothing about `sending` or `actionError`. */
  const activeSend = useRef<{ cardId: string; echoId: string } | null>(null);
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
  const failedSend = registry.failedSends[cardId] ?? null;
  // A stale cache can reveal an old equal message after this attempt. Only the
  // reader may dismiss its recovery state; a match is a review hint, not an ack.
  const matchingSendMessage = failedSend?.delivery === 'unknown'
    && hasUnseenMatchingConversationMessage(serverTurns, failedSend.echo);
  useEffect(() => {
    setEchoes([]);
    setUnconfirmedEchoId(null);
    setActionError(null);
    /* The send in flight belongs to the conversation being left; its own answer is still delivered. */
    activeSend.current = null;
    sendingRef.current = false;
    setSending(false);
    setInterruptPending(false);
  }, [cardId]);
  /* A send can settle through an older store after this card is already open in
     a new one. Merge its confirmed optimistic turn from the provider, then give
     every newer server row to the oldest eligible echo exactly once. */
  useEffect(() => {
    const remembered = registry.turnsOf(cardId).filter(isOptimisticConversationTurn);
    setEchoes((current) => {
      const present = new Set(current.map((turn) => turn.id));
      const additions = remembered.filter((turn) => !present.has(turn.id));
      const merged = additions.length === 0
        ? current
        : [...current, ...additions].toSorted((left, right) => left.atMs - right.atMs);
      const next = reconcileOptimisticConversationTurns(serverTurns, merged);
      return next.length === current.length && next.every((turn, index) => turn === current[index])
        ? current
        : next;
    });
  }, [cardId, registry, serverTurns]);

  const turns = useMemo(
    () => [...serverTurns, ...echoes].sort((left, right) => left.atMs - right.atMs),
    [echoes, serverTurns],
  );
  /* The same turns, minus the one nobody has agreed to yet. */
  const confirmedEchoes = useMemo(
    () => unconfirmedEchoId === null
      ? echoes
      : echoes.filter((turn) => turn.id !== unconfirmedEchoId),
    [echoes, unconfirmedEchoId],
  );
  const confirmedTurns = useMemo(
    () => [...serverTurns, ...confirmedEchoes].sort((left, right) => left.atMs - right.atMs),
    [confirmedEchoes, serverTurns],
  );
  /* An echo belongs after everything the server has confirmed; a completed action
       keeps the started row's place. */
  const transcript = useMemo(
    () => {
      // A phase snapshot predicts queueing; only this POST's acknowledgement
      // licenses the queued caption. A wedged queue cannot promise delivery.
      const displayedEchoes = echoes
        /* An echo that has claimed an entry id the queue region is listing is drawn
                   there, not here; reversible the moment the entry drains. */
        .filter((turn) => turn.entryId === null || !pendingQueueIds.has(turn.entryId))
        .map((turn) => stalled || turn.id === unconfirmedEchoId
          ? { ...turn, queued: false } : turn);
      return mergeTranscript(serverEntries, displayedEchoes);
    },
    [echoes, pendingQueueIds, serverEntries, stalled, unconfirmedEchoId],
  );
  const confirmedTranscript = useMemo(
    () => mergeTranscript(serverEntries, confirmedEchoes), [confirmedEchoes, serverEntries],
  );
  const working = phase === 'issuing_turn' || phase === 'turn_running';
  const stopping = !stalled && (phase === 'issuing_interrupt' || interruptPending);
  const facts = useMemo<ConversationFacts | null>(() => trackId === undefined ? null : {
    cardId, trackId, trackTitle, cardTitle: cardTitle ?? null, kind: scopeKind,
    state: scopeState, working, stalled, fallbackUpdatedAt: scopeUpdatedAt ?? 0,
  }, [cardId, cardTitle, scopeKind, scopeState, scopeUpdatedAt, trackId, trackTitle, working, stalled]);
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
    /* The confirmed transcript: a message that may still fail is not part of what this conversation is. */
    registry.remember(durableConversation, confirmedTranscript);
  }, [confirmedTranscript, durableConversation, registry, rememberOn]);
  useEffect(() => {
    /* A `'rows'` route remembers every row it lists; `rememberOn` is compared against
         each row so another Track's row cannot write into this scope. */
    for (const row of serverRows) {
      if (row.trackId !== rememberOn) continue;
      /* The open row belongs to the effect above; writing the plain row over it here
               would undo it on every render. */
      if (row.id === conversation?.id) continue;
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
  }, [conversation?.id, registry, rememberOn, serverRows]);

  const listedConversations = useMemo(
    () => serverRows.map((row) => withRememberedTitle(
      row, registry.conversations.find((candidate) => candidate.id === row.id),
    )),
    [registry.conversations, serverRows],
  );
  /* The open row is replaced in place by the live one: same id, plus the turns and
       name only the transcript can supply. */
  const conversations = conversation === null
    ? listedConversations
    : listedConversations.map((row) => row.id === conversation.id ? conversation : row);

  const send = async (
    _conversationId: string, text: string, attachments: readonly PlannerAttachment[] = [],
  ): Promise<SendOutcome> => {
    if (_conversationId !== cardId || stalled || sendingRef.current || !registry.tryBeginSend(cardId)) return 'not-sent';
    sendingRef.current = true;
    setSending(true);
    setActionError(null);
    const echo: OptimisticConversationTurn = {
      id: `echo-${mintIdempotencyKey()}`, author: 'you' as const, text, atMs: Date.now(),
      /* The echo carries the images: an image-only message has no text to reconcile
               on, so the ids are the second criterion. */
      attachments,
      serverHighWaterBefore: serverItemHighWater(items),
      /* Read at the press from the last `GET /planner/run` snapshot, against the
               kernel's whitelist (`can_issue_turn()`), not `working`: a front-end notion of
               "busy" is not the kernel's notion of "can start a turn". Never recomputed
               from the live phase later. */
      queued: kernelQueuesInput(phase),
      /* Not knowable yet — the POST below is what answers it. Claimed in the
         `then`, and left `null` forever if the server has none to give. */
      entryId: null,
    };
    const sentTo = cardId;
    activeSend.current = { cardId: sentTo, echoId: echo.id };
    /* Still ours to answer for. False from the moment the reader moved to
       another conversation (the `cardId` effect) or started a later send. */
    const stillActive = () => activeSend.current?.echoId === echo.id;
    let sendFailure: FailedConversationSend | null = null;
    /* Decided where the fact is known and read once at the end; `sendFailure` is
           set before the `stillActive()` guard so cannot stand in for it. */
    let settled: SendOutcome = 'delivered';
    /* Set inside `finally`, where `stillActive()` is asked before it is
       cleared. */
    let answeredHere = false;
    setEchoes((current) => [...current, echo]);
    setUnconfirmedEchoId(echo.id);
    return mutations.send(text, attachments.map((attachment) => attachment.id)).then((sent) => {
      setUnconfirmedEchoId((current) => current === echo.id ? null : current);
      /* The claim decides only who draws this message; written wherever the echo
               still lives, the registry unconditionally. */
      const claimedEntryId = sent.entry_id;
      if (claimedEntryId !== null && stillActive()) {
        setEchoes((current) => current.map((turn) =>
          turn.id === echo.id ? { ...turn, entryId: claimedEntryId } : turn));
      }
      /* The answer can outlive the drawer: with `scope` null the effects above stop
               writing, so the confirmation is written straight through for the
               conversation it was sent to. Through `updateExisting`, not `remember`: a
               background refresh may already have put newer data in this entry. */
      registry.updateExisting(sentTo, ({ conversation: known, turns: knownTurns }) => {
        /* The refresh may already have brought this message back; reconcile all
                   optimistic turns together, oldest first, so one server row confirms one echo. */
        const claimed = { ...echo, entryId: claimedEntryId };
        const remembered = knownTurns
          .filter(isOptimisticConversationTurn)
          .map((turn) => turn.id === echo.id ? claimed : turn);
        const optimistic = remembered.some((turn) => turn.id === echo.id)
          ? remembered
          : [...remembered, claimed].toSorted((left, right) => left.atMs - right.atMs);
        const serverMessages = knownTurns.filter((turn): turn is ConversationMessage =>
          isConversationMessage(turn) && !isOptimisticConversationTurn(turn));
        const unresolved = reconcileOptimisticConversationTurns(serverMessages, optimistic);
        const unresolvedIds = new Set(unresolved.map((turn) => turn.id));
        const recorded = !unresolvedIds.has(echo.id);
        const nextTurns = knownTurns
          .filter((turn) => !isOptimisticConversationTurn(turn) || unresolvedIds.has(turn.id))
          /* The remembered copy may predate the claim; the claim is the only
             difference and it is this write's whole point. */
          .map((turn) => turn.id === echo.id ? claimed : turn);
        if (!recorded && !nextTurns.some((turn) => turn.id === echo.id)) nextTurns.push(claimed);
        return {
          conversation: {
            ...known,
            title: known.title ?? conversationNameFrom(text),
            updatedAt: Math.max(known.updatedAt, echo.atMs),
            turns: nextTurns.filter(isConversationMessage).length,
          },
          turns: nextTurns,
        };
      });
    }).catch((error: unknown) => {
      settled = isSendRefusalCode(apiFailureCodeOf(error)) ? 'refused' : 'unresolved';
      sendFailure = {
        echo, message: errorMessage(error, 'Could not send the message.'),
        delivery: settled === 'refused' ? 'refused' : failedConversationDelivery(error instanceof ApiError ? error.failure : null),
      };
      /* A failure belongs to the conversation that failed; the provider still records
               it for a remount of the owning card. */
      if (!stillActive()) return;
      setEchoes((current) => current.filter((turn) => turn.id !== echo.id));
    }).finally(() => {
      setUnconfirmedEchoId((current) => current === echo.id ? null : current);
      registry.finishSend(sentTo, sendFailure);
      /* Re-opening the composer is a statement about the send in flight now; made
               unconditionally, a stale request could clear an unanswered send's flag. */
      if (!stillActive()) return;
      answeredHere = true;
      activeSend.current = null;
      sendingRef.current = false;
      setSending(false);
    }).then((): SendOutcome => answeredHere ? settled : 'abandoned');
  };

  const interrupt = () => {
    if (!working || stopping) return;
    setInterruptPending(true);
    setActionError(null);
    void mutations.interrupt().catch((error: unknown) => {
      setActionError(errorMessage(error, 'Could not stop the turn.'));
    }).finally(() => setInterruptPending(false));
  };

  const sendingAcrossMounts = cardId !== '' && registry.pendingSendIds.has(cardId);
  /**
   * Take one message out of the transcript for good: a deleted queue entry never
   * becomes a transcript row, so reconciliation can never retire its echo. Both
   * copies, since the registry's outlives this mount.
   */
  const retireQueuedEcho = (entryId: string): void => {
    const isRetired = (turn: TranscriptEntry) =>
      isOptimisticConversationTurn(turn) && turn.entryId === entryId;
    setEchoes((current) => current.filter((turn) => !isRetired(turn)));
    registry.updateExisting(cardId, ({ conversation: known, turns: knownTurns }) => ({
      conversation: known,
      turns: knownTurns.filter((turn) => !isRetired(turn)),
    }));
  };
  const deleteQueuedEntry = (entry: PendingQueueEntry) =>
    mutations.deleteQueued(entry.entry_id, entry.rev).then((outcome) => {
      /* `gone` is not a retirement: the entry drained and its transcript row is on its way. */
      if (outcome.kind === 'done') {
        retireQueuedEcho(entry.entry_id);
        forgetQueuedEntry(entry);
      }
      return outcome;
    });
  /* On a steer's `done` the entry is forgotten but its echo is NOT retired: a steer
       delivers the sentence, and the kernel's transcript row reconciles the echo. */
  const steerQueuedEntry = phase === 'turn_running'
    ? (entry: PendingQueueEntry) =>
      mutations.steerQueued(entry.entry_id, entry.rev).then((outcome) => {
        if (outcome.kind === 'done') forgetQueuedEntry(entry);
        return outcome;
      })
    : undefined;

  /* A queued echo cannot be waited on: the pending queue writes no transcript row
       until the turn ends, so counting it would kill the composer for a whole turn.
       This is not the duplicate-submit guard; that is `sending` above. */
  const awaitsReconciliation = (turn: TranscriptEntry) =>
    isOptimisticConversationTurn(turn) && !turn.queued;
  const hasUnreconciledSend = echoes.some(awaitsReconciliation)
    || registry.turnsOf(cardId).some(awaitsReconciliation);
  const sendBlocked = stalled || (failedSend !== null && failedSend.delivery !== 'refused') || sending || sendingAcrossMounts || hasUnreconciledSend;
  const displayedFailure = failedSend === null ? null : { ...failedSend.echo, queued: false };
  return {
    conversations,
    turnsOf: (conversationId) => conversation?.id === conversationId
      ? displayedFailure === null || matchingSendMessage ? transcript
        : mergeTranscript(transcript, [displayedFailure])
      : registry.turnsOf(conversationId),
    pending: pendingConversationIds(conversation, working, !stalled && (sending || sendingAcrossMounts)),
    working,
    stalled,
    stopping,
    sending: sending || sendingAcrossMounts,
    sendBlocked,
    pendingQueue,
    pendingQueueOverflow,
    deleteQueuedEntry,
    steerQueuedEntry,
    historyReady: history.data !== undefined,
    historyLoading: history.isFetching,
    hasEarlier: history.hasNextPage,
    loadingEarlier: history.isFetchingNextPage,
    historyError: history.error instanceof Error ? history.error.message : null,
    actionError,
    failedSend,
    matchingSendMessage,
    retrySend: (echoId) => {
      /* The retry carries the echo's images: an image-only message re-sent as
             `{ text: "" }` is refused, and the ids on a failed echo are still bound. */
      if (failedSend?.echo.id === echoId) {
        void send(cardId, failedSend.echo.text, failedSend.echo.attachments);
      }
    },
    send: (conversationId, text, attachments) => failedSend === null || failedSend.delivery === 'refused'
      ? send(conversationId, text, attachments) : Promise.resolve('not-sent'),
    attachmentsSupported: run.data?.attachments_supported ?? false,
    contextUsage: run.data?.token_usage ?? null,
    uploadAttachment: mutations.uploadAttachment,
    interrupt,
    retryHistory: () => { void history.refetch().catch(() => undefined); },
    loadEarlier: () => { void history.fetchNextPage().catch(() => undefined); },
    blockedReason: run.data?.blocked_reason ?? null,
    /* Before the first answer the conversation is following the default, which is
           what a card with no selection really does. */
    model: run.data === undefined
      ? FOLLOW_INSTALLATION_DEFAULT
      : { model: run.data.model, reasoning_effort: run.data.reasoning_effort },
    modelCatalog: modelCatalog.data ?? null,
    setModel: (selection) => {
      setActionError(null);
      void mutations.setModel(selection)
        .then((result) => {
          /* Both flags are reported: the write succeeded, but the value stored is not
                       quite the value asked for. */
          if (result.effort_adjusted) {
            setActionError(
              `That reasoning effort is not available on this model; it now uses ${result.reasoning_effort ?? 'the default'}.`,
            );
          } else if (result.unknown_model) {
            setActionError('codex does not list that model for this account. It is saved; turns may fail.');
          }
        })
        .catch((error: unknown) => {
          setActionError(errorMessage(error, 'Could not change the model.'));
        });
    },
  };
}

/**
 * The one conversation whose transcript is being read. `id` is the Track the card
 * hangs off; `state` is the row's server state as the baseline, and only the open
 * row also picks up the local phase and the name derived from its first message.
 */
type PlannerConversationScope = Readonly<{
  id: string;
  title?: string;
  cardId: string;
  cardTitle: string | null;
  updatedAt: number;
  kind?: ConversationKind;
  state?: ConversationState | null;
}>;

/** A route-owned server list whose rows open in this panel's drawer. */
type ConversationPanelSource = Readonly<{
    /** The Track this draft belongs to. */
    scopeId: string;
    rows: readonly Conversation[];
    /** The kernel's per-card verdicts for the track these rows are on; required, because a caller that passes no overlay has a list on which nothing can ever be working. */
    cards: Readonly<Record<string, CardActivity>>;
    /** The Track these rows may be sent to. */
    rememberOn: string;
    scopeOf: (conversationId: string) => PlannerConversationScope | null;
    /** The id the card minted under this key will have, derived before the POST. */
    derivedCardId: (idempotencyKey: string) => string;
    create: (text: string, idempotencyKey: string, selection: ModelSelection) => Promise<Conversation>;
    refresh: () => Promise<readonly Conversation[]>;
  }>;

/** What a caller may change without touching the draft's identity: `key` and
 *  `sentText` move together or not at all, through `rekeyDraft` and `markDraftSent` only. */
type DraftEdit = Partial<Pick<ConversationDraft, 'text' | 'creating' | 'error' | 'remedy'>>;

/** The card runtime, created once at boot and injected. */
export type CardRuntime = Readonly<{ registry: CardRegistry; host: CardHost }>;

export type AppRouterDeps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  client: QueryClient;
  onSignOut: () => void;
  cards: CardRuntime;
  recentFiles?: RecentFileHistory;
  uiPreferences?: UiPreferences;
}>;

function renderNothing(): null { return null; }

export function createRouteTree(deps: AppRouterDeps): AnyRoute {
  const { transport, unauthorized, client, onSignOut, cards } = deps;
  const recentFiles = deps.recentFiles ?? createRecentFileHistory();
  const preferences = deps.uiPreferences ?? createUiPreferences();
  const rootRoute = createRootRoute({ component: () => (
    <UiPreferencesProvider preferences={preferences}>
      <TrackViewProvider><ShellRoute transport={transport} unauthorized={unauthorized} onSignOut={onSignOut} /></TrackViewProvider>
    </UiPreferencesProvider>
  ) });

  const indexRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/',
    /** The index loader primes only the areas list; awaiting the area → tracks fan-out here would let one slow area block the route commit. */
    loader: () => prefetchAreaList(client, transport, unauthorized),
    component: () => <TodayRoute transport={transport} unauthorized={unauthorized} />,
  });

  const newTrackRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/area/$areaId/new',
    component: () => <NewTrackRoute transport={transport} unauthorized={unauthorized} />,
  });

  const trackRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/track/$trackId',
    validateSearch: (search: Record<string, unknown>): TrackSearch => validateTrackSearch(search),
    component: () => <TrackRoute
      transport={transport}
      unauthorized={unauthorized}
      cardRuntime={cards}
      recentFiles={recentFiles}
    />,
  });

  const recipesRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/recipes',
    component: () => <RecipesRoute transport={transport} unauthorized={unauthorized} />,
  });

  /* Every settings route renders nothing: the URL is the state and `SettingsOverlay`
       is its view. A route component remounts on every navigation, which would replay
       the panel's entrance animation on every click. */
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings',
    component: renderNothing,
  });

  const generalRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/general',
    component: renderNothing,
  });

  const networkRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/network',
    component: renderNothing,
  });

  const pluginsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/plugins',
    component: renderNothing,
  });

  const appearanceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/appearance',
    component: renderNothing,
  });

  const aboutRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/about',
    component: renderNothing,
  });

  return rootRoute.addChildren([
    indexRoute, newTrackRoute, trackRoute, recipesRoute, settingsRoute,
    generalRoute, networkRoute, pluginsRoute, appearanceRoute, aboutRoute,
  ]);
}

export function createAppRouter(deps: AppRouterDeps) {
  return createRouter({
    routeTree: createRouteTree(deps),
    basepath: APP_BASEPATH,
    defaultPreload: false,
  });
}

function ShellRoute({ transport, unauthorized, onSignOut }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onSignOut: () => void }) {
  const go = useGo();
  return (
    <ConversationProvider><NewTrackDraftProvider>
      <AppShell
        transport={transport}
        unauthorized={unauthorized}
        onOpenSettings={() => go({ name: 'settings' })}
        onOpenPlugins={() => go({ name: 'settings-plugins' })}
        onSignOut={onSignOut}
      />
    </NewTrackDraftProvider></ConversationProvider>
  );
}

/**
 * The conversation module shared by Today and Track. A draft is a third open state,
 * not a `Conversation`: the card is minted by the first message, so until one is
 * sent there is no card id and nothing to fetch.
 */
function useConversationPanel(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
  source: ConversationPanelSource,
  options?: { showTrack?: boolean },
) {
  /* Existing conversation selection survives navigation; unfinished drafts
     retain their separate ConversationProvider lifecycle. */
  const [openTarget, setOpenTarget] = useConversationViewTarget(source.scopeId);
  /* The conversation whose composer this route was asked to put the caret in. Held
       here because the request is cleared in the same commit that opens the row;
       dropped when the drawer closes. */
  const [composerFocusFor, setComposerFocusFor] = useState<string | null>(null);
  const [resendConfirmation, setResendConfirmation] = useState<string | null>(null);
  const [composerDraft, setComposerDraft] = useState('');
  const openRowId = openTarget?.kind === 'row' ? openTarget.id : null;
  const draftCatalog = useQuery({ ...modelCatalogQueryOptions(transport, null, unauthorized),
    enabled: openTarget?.kind === 'draft' });
  const draftCapabilities = useQuery({ queryKey: ['server-version'],
    queryFn: () => runOperation(transport, serverVersionOperation(), unauthorized),
    enabled: openTarget?.kind === 'draft', retry: false });
  const supportsDraftModel = draftCapabilities.data?.conversationCreateModel === true;
  useEffect(() => { if (openRowId === null) setComposerFocusFor(null); }, [openRowId]);
  const scope: PlannerConversationScope | null = openRowId !== null
    ? source.scopeOf(openRowId)
    : null;
  const routeIntent: ConversationRouteIntent = {
    rows: source.rows, rememberOn: source.rememberOn,
  };


  const rows = source.rows;
  const store = useConversationStore(transport, unauthorized, scope, routeIntent);
  /* The composer's pending images, keyed to the open card so moving to another
       conversation does not carry a picked image into it. */
  const attachments = usePlannerAttachments(store.uploadAttachment, scope?.cardId ?? '');
  const registry = useConversationRegistry();
  const go = useGo();
  const open = store.conversations.find((conversation) => conversation.id === openRowId) ?? null;
  const preferences = useUiPreferences();
  // Receipts compare the row's completion time, not `updatedAt`, which also moves
  // when the reader queues a message. `null` is never unread.
  const openActivity = rows.find(row => row.id === open?.id);
  useReadReceipt('conversation', openActivity?.id ?? null, openActivity?.lastTurnCompletedAt ?? 0,
    store.historyReady && !store.historyLoading && store.historyError === null);

  /* Only this route's slot is visible, reopenable or sendable here. */
  const sourceScopeId = source.scopeId;
  const draft = registry.draftOf(sourceScopeId);
  const adoptedDraftId = registry.adoptedDraftIdOf(sourceScopeId);
  const creating = draft?.creating ?? false;
  const discardUnsentDraft = registry.discardUnsentDraft;

  /* Preserve only a draft whose request actually left the browser; an untouched or
       locally refused draft has no server identity. */
  useEffect(() => {
    return () => { discardUnsentDraft(sourceScopeId); };
  }, [discardUnsentDraft, sourceScopeId]);

  /* Adoption: the reducer moves a matching `{ scopeId, key }` from `held` to
       `adopted`; if the create settles while unmounted, the outcome waits here. */
  useEffect(() => {
    if (adoptedDraftId === null) return;
    if (!rows.some((row) => row.id === adoptedDraftId)) return;
    setOpenTarget({ kind: 'row', id: adoptedDraftId });
    registry.finishDraftAdoption(sourceScopeId, adoptedDraftId);
  }, [adoptedDraftId, registry, rows, sourceScopeId, setOpenTarget]);

  /* Every draft write goes through one of these three, each a whole-object update,
       and each a no-op when the draft it was computed from is no longer the one held. */
  const withDraft = (
    from: ConversationDraftId, next: (current: ConversationDraft) => ConversationDraft,
  ) => {
    registry.editDraft(from, next);
  };
  const amendDraft = (from: ConversationDraft, change: DraftEdit) => {
    withDraft(from, (current) => ({ ...current, ...change }));
  };
  /* The only way to change the key, and it always clears `sentText`: a key is the
       identity of an attempt and `sentText` is what that attempt sent. */
  const rekeyDraft = (from: ConversationDraft, key: string, change: DraftEdit = {}): ConversationDraft => {
    const next = { ...from, ...change, key, sentText: null };
    withDraft(from, (current) => ({ ...current, ...change, key, sentText: null }));
    return next;
  };
  /** Records that a POST is going out under this key with these words. Called
   *  before the request, so a failure finds the right baseline. */
  const markDraftSent = (from: ConversationDraft, text: string) => {
    withDraft(from, (current) => ({ ...current, text, sentText: text }));
  };

  /* Consumed only when the rows are loaded AND contain the id, never cleared on
       absence: the list arrives a round trip after the request, and the id may
       belong to another track. */
  useEffect(() => {
    const requestedOpenId = registry.requestedOpenId;
    if (requestedOpenId === null) return;
    /* Captured here, not read at render time: the request is cleared in the same
           commit that opens the row. */
    const focusComposer = registry.requestedOpenFocusesComposer;
    if (!rows.some((row) => row.id === requestedOpenId)) return;
    setOpenTarget({ kind: 'row', id: requestedOpenId });
    if (focusComposer) setComposerFocusFor(requestedOpenId);
    registry.clearOpenRequest();
  }, [registry, rows, setOpenTarget]);

  useEffect(() => {
    if (open === null) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
      if (!store.working || store.stopping) return;
      const target = event.target;
      if (!(target instanceof Element)) return;
      const region = target.closest('[role="complementary"]');
      if (region === null) return;
      /* The source panel is a second `complementary` on the same track, and its Escape
               must not reach the planner; the region is asked whether it holds the panel's marker. */
      if (region.querySelector('[data-nc-report-source]') !== null) return;
      /* An open `/` menu owns Escape first; this capture-phase listener would otherwise
               take it. The menu says it is open through `aria-expanded` on the combobox. */
      if (target.closest('[role="combobox"][aria-expanded="true"]') !== null) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      store.interrupt();
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, [open, store]);

  /* The `+` opens a draft scoped to one concrete Track; Today materialises the
       launchpad before calling this, so the scope id is never empty. */
  const start = () => {
    setComposerDraft('');
    /* A draft that was sent and failed is still open business: reopened with the
           same key, so the next attempt is a retry and not a second conversation. */
    if (draft !== null && draft.sentText !== null) {
      setOpenTarget({ kind: 'draft' });
      return;
    }
    /* The key is minted once, for the draft, not per send: a different key on the
           retry is a different derived card. */
    registry.startDraft({
      scopeId: source.scopeId,
      model: FOLLOW_INSTALLATION_DEFAULT,
      key: mintIdempotencyKey(),
      text: null, sentText: null, creating: false, error: null, remedy: null,
    });
    setOpenTarget({ kind: 'draft' });
  };

  const startAnother = start;
  const continueFromStall = () => {
    start();
    // Starting another conversation is a recovery handoff: keep the words
    // the reader was composing, ready to edit before any request is sent.
    setComposerDraft(composerDraft);
  };

  /* `from` is not decoration: this runs after an `await`, and the reducer records
       the row only if `from` is still held. */
  const adopt = (from: ConversationDraftId, row: Conversation) => {
    registry.adoptDraft(from, row.id);
    /* Nothing is minted for the first sentence here: the kernel writes it to the
           transcript at drain, and the item read serves it back. */
  };

  const UNCONFIRMED = 'Could not check whether the last attempt went through. Try again in a moment.';

  /* Re-read the list and adopt this draft's OWN row (by derived id), never "the
       list grew". Three answers: `'unknown'` is the re-read itself failing, and
       treating it as `'absent'` would mint a new key over an attempt that may have
       committed. */
  const adoptIfItLanded = async (
    refresh: () => Promise<readonly Conversation[]>,
    derivedCardId: (idempotencyKey: string) => string,
    scopeId: string,
    key: string,
    current: () => boolean,
  ): Promise<'landed' | 'absent' | 'unknown'> => {
    if (!current()) return 'unknown';
    const rows = await refresh().catch(() => null);
    if (!current() || rows === null) return 'unknown';
    const cardId = derivedCardId(key);
    const landed = rows.find((row) => row.id === cardId);
    if (landed === undefined) return 'absent';
    adopt({ scopeId, key }, landed);
    return 'landed';
  };

  /* The draft's own send: it runs while there is no card, and its text lives in the
       registry's draft entry until the row it created is adopted. */
  const refuseOfflineDraft = (attempt: ConversationDraft, text: string): boolean => {
    if ((attempt.model.model !== null || attempt.model.reasoning_effort !== null) && !supportsDraftModel) {
      amendDraft(attempt, { text, error: 'This server does not support choosing the first message’s model yet.', remedy: 'retry' });
      return true;
    }
    if (onlineManager.isOnline()) return false;
    amendDraft(attempt, { text, error: new OfflineSubmissionError().message, remedy: 'retry' });
    return true;
  };

  const admitDraft = (attempt: ConversationDraft): (() => boolean) | null => {
    try {
      const admitted = admitTransport(transport);
      const checkpoint = admitted.recovery?.checkpoint();
      return () => { try { checkpoint?.(); return true; } catch { return false; } };
    } catch (error) {
      amendDraft(attempt, { error: errorMessage(error, 'Connection is not ready. Try again after reconnecting.'), remedy: 'retry' });
      return null;
    }
  };

  const sendDraft = (text: string) => {
    if (creating || draft === null) return;
    const { create, refresh, scopeId, derivedCardId } = source;
    /* The server refuses `text.trim().is_empty()` but counts `chars()` on the
           untrimmed text, in Unicode scalar values: so the blank check trims, the
           length check does not, and `Array.from` counts code points. */
    if (text.trim() === '') return;
    if (Array.from(text).length > CONVERSATION_TEXT_MAX) {
      /* Shown back, but never recorded as sent: no request left the browser, so
         the key is untouched and the next press is not "the text changed". */
      amendDraft(draft, {
        text,
        error: `This message is too long — the limit is ${CONVERSATION_TEXT_MAX} characters.`,
        remedy: null,
      });
      return;
    }
    if (refuseOfflineDraft(draft, text)) return;
    const current = admitDraft(draft); if (current === null) return;
    const previousText = draft.sentText;
    /* The draft this send is for, fixed here: a send that outlives its draft changes nothing. */
    let attempt = draft;
    let previouslySentText = attempt.sentText;
    amendDraft(attempt, { text, creating: true, error: null, remedy: null });
    void (async () => {
      try {
        /* Editing the text after a failure has to look at the list first: the old key
                 may have succeeded with the old text, and only a re-read saying "no new
                 row" earns a new key. */
        if (previousText !== null && previousText !== text) {
          const landing = await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key, current);
          if (landing === 'landed') return;
          if (landing === 'unknown') {
            amendDraft(attempt, { error: UNCONFIRMED, remedy: 'retry' });
            return;
          }
          attempt = rekeyDraft(attempt, mintIdempotencyKey());
        }
        if (!current()) { amendDraft(attempt, { error: 'Connection changed. Retry after reconnecting.', remedy: 'retry' }); return; }
        if (refuseOfflineDraft(attempt, text)) return;
        previouslySentText = attempt.sentText;
        markDraftSent(attempt, text);
        attempt = { ...attempt, text, sentText: text };
        const created = await create(text, attempt.key, attempt.model);
        if (current()) adopt(attempt, created);
        else amendDraft(attempt, { error: 'Connection changed. Retry after reconnecting.', remedy: 'retry' });
      } catch (error: unknown) {
        if (error instanceof OfflineSubmissionError) {
          // Marking a request optimistically must not invent dispatch when the
          // mutation's later guard refused it. Keep any earlier unknown send.
          registry.editDraft(attempt, (current) => ({ ...current, sentText: previouslySentText }));
          amendDraft(attempt, { error: error.message, remedy: 'retry' });
        } else {
          attempt = await handleCreateFailure(error, refresh, derivedCardId, scopeId, attempt, current);
        }
      } finally {
        amendDraft(attempt, { creating: false });
      }
    })();
  };

  async function handleCreateFailure(
    error: unknown,
    refresh: () => Promise<readonly Conversation[]>,
    derivedCardId: (idempotencyKey: string) => string,
    scopeId: string,
    attempt: ConversationDraft,
    current: () => boolean,
  ): Promise<ConversationDraft> {
    if (!current()) {
      amendDraft(attempt, { error: 'Connection changed. Retry after reconnecting.', remedy: 'retry' });
      return attempt;
    }
    const failure = error instanceof ApiError
      ? conversationCreateFailure(error.failure)
      : { kind: 'retry' as const, message: errorMessage(error, 'Could not start the conversation.') };
    const message = failure.message;
    switch (failure.kind) {
      case 'gone':
        registry.discardDraft(attempt);
        go({ name: 'today' });
        return attempt;
      case 'exhausted':
        /* A spent key can never succeed again, so a new one is minted and takes
                   `sentText` with it: nothing was posted under this key. */
        return rekeyDraft(attempt, mintIdempotencyKey(), { error: message, remedy: 'retry' });
      case 'stale-payload':
        amendDraft(attempt, { error: message, remedy: 'new-conversation' });
        return attempt;
      case 'blocked':
        /* Nothing committed and the key is unspent, so both it and the words are kept. */
        amendDraft(attempt, { error: message, remedy: 'retry' });
        return attempt;
      case 'exists': {
        /* The derived card exists, so this key can never mint again; a new key is
                   offered only once the re-read has said there is no row. */
        amendDraft(attempt, { error: message });
        const landing = await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key, current);
        if (landing === 'absent') amendDraft(attempt, { remedy: 'new-conversation' });
        if (landing === 'unknown') amendDraft(attempt, { error: UNCONFIRMED, remedy: 'retry' });
        return attempt;
      }
      case 'unavailable':
      case 'retry':
        /* Both are ambiguous: on this endpoint a 503 is usually raised after the card
                 is minted, so the look for this key's card is not skipped. */
        amendDraft(attempt, { error: message });
        if (await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key, current) !== 'landed') {
          amendDraft(attempt, { remedy: 'retry' });
        }
        return attempt;
    }
  }

  const sendAsNewConversation = () => {
    if (creating || draft === null || draft.text === null) return;
    const { create, refresh, scopeId, derivedCardId } = source;
    const text = draft.text;
    if (refuseOfflineDraft(draft, text)) return;
    const current = admitDraft(draft); if (current === null) return;
    let attempt = draft;
    let previouslySentText = attempt.sentText;
    amendDraft(attempt, { creating: true, error: null, remedy: null });
    void (async () => {
      try {
        /* Pressed deliberately, but the same fence applies: a new key is only
           safe once the list has actually said the old one produced nothing. */
        const landing = await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key, current);
        if (landing === 'landed') return;
        if (landing === 'unknown') {
          amendDraft(attempt, { error: UNCONFIRMED, remedy: 'new-conversation' });
          return;
        }
        attempt = rekeyDraft(attempt, mintIdempotencyKey());
        if (!current()) { amendDraft(attempt, { error: 'Connection changed. Retry after reconnecting.', remedy: 'retry' }); return; }
        if (refuseOfflineDraft(attempt, text)) return;
        previouslySentText = attempt.sentText;
        markDraftSent(attempt, text);
        attempt = { ...attempt, text, sentText: text };
        const created = await create(text, attempt.key, attempt.model);
        if (current()) adopt(attempt, created);
        else amendDraft(attempt, { error: 'Connection changed. Retry after reconnecting.', remedy: 'retry' });
      } catch (error: unknown) {
        if (error instanceof OfflineSubmissionError) {
          // Marking a request optimistically must not invent dispatch when the
          // mutation's later guard refused it. Keep any earlier unknown send.
          registry.editDraft(attempt, (current) => ({ ...current, sentText: previouslySentText }));
          amendDraft(attempt, { error: error.message, remedy: 'retry' });
        } else {
          attempt = await handleCreateFailure(error, refresh, derivedCardId, scopeId, attempt, current);
        }
      } finally {
        amendDraft(attempt, { creating: false });
      }
    })();
  };

  /* Retry means the same draft again: same key, same words. */
  const retryDraft = () => {
    if (draft === null || draft.text === null) return;
    sendDraft(draft.text);
  };

  /* A draft no request was ever made for has no identity worth keeping; one that
       was sent and failed keeps its key and words so the next attempt is a retry. */
  const closeDrawer = () => {
    setOpenTarget(null);
    setComposerDraft('');
    if (draft !== null && draft.sentText === null) registry.discardDraft(draft);
  };

  /* A draft belonging to another Track is not open here: `draft` is read only
     from this route's provider slot. */
  const draftOpen = openTarget?.kind === 'draft' && draft !== null;

  return {
    isOpen: open !== null || draftOpen,
    close: closeDrawer,
    list: (
      <ChatList
        conversations={store.conversations}
        cards={source.cards}
        unreadIds={new Set(rows.filter(row => preferences.isUnread('conversation', row.id, row.lastTurnCompletedAt ?? 0)).map(row => row.id))}
        activeId={open?.id ?? null}
        /* The two local echoes for the open row only, handed over as facts: the list
                   reads nothing off `Conversation.state`. */
        local={open === null ? null : { id: open.id, working: store.working, stalled: store.stalled }}
        showTrack={options?.showTrack ?? true}
        onOpen={(conversation) => {
          setOpenTarget({ kind: 'row', id: conversation.id });
        }}
      />
    ),
    action: <PanelAction label="New conversation" onClick={start}><Icon name="plus" size="sm" /></PanelAction>,
    startConversation: start,
    drawer: (
      <Drawer
        open={open !== null || draftOpen}
        /* A draft has no name yet, and naming it after the words being typed
           would rename the drawer on every keystroke. */
        title={open !== null ? conversationName(open) : draftOpen ? 'Untitled' : ''}
        mobileBackLabel="Conversations"
        onClose={closeDrawer}
        footer={draftOpen ? (
          <>
            {/* The strip is welded to the well's top edge, so it renders before the composer. */}
            {draft != null && (draft.error != null || draft.remedy !== null) && (
              <ChatFooterNotice>
                {draft.error != null && <ChatFooterError message={draft.error} />}
                {draft.remedy === 'retry' && (
                  <ChatFooterRemedy disabled={creating} onClick={retryDraft}>Try again</ChatFooterRemedy>
                )}
                {draft.remedy === 'new-conversation' && (
                  <ChatFooterRemedy disabled={creating} onClick={sendAsNewConversation}>
                    Send as a new conversation
                  </ChatFooterRemedy>
                )}
              </ChatFooterNotice>
            )}
            <ChatComposer disabled={creating} onSend={sendDraft} onNewConversation={startAnother}
              draft={{ text: composerDraft, onChange: setComposerDraft }}
              footerActions={<ModelPill catalog={draftCatalog.data ?? null} selection={draft.model}
                onChange={model => withDraft(draft, current => current.creating || current.sentText !== null
                  ? current : { ...current, model })}
                isDisabled={creating || draft.sentText !== null || !supportsDraftModel} />} />
          </>
        ) : open === null ? undefined : (
          <>
            {store.historyError !== null && (
              <ChatFooterNotice>
                <ChatFooterError message={store.historyError} />
                <ChatFooterRemedy disabled={store.historyLoading} onClick={store.retryHistory}>
                  {store.historyLoading ? 'Loading…' : 'Try again'}
                </ChatFooterRemedy>
              </ChatFooterNotice>
            )}
            {store.stalled && (
              <ChatFooterNotice>
                <ChatFooterError message="This conversation is stuck. Start a new conversation to continue." />
                <ChatFooterRemedy onClick={continueFromStall}>Start a new conversation</ChatFooterRemedy>
              </ChatFooterNotice>
            )}
            {store.failedSend !== null && (
              <ChatFooterNotice tone={store.matchingSendMessage ? 'neutral' : 'error'}>
                {store.matchingSendMessage ? (
                  <span>A matching message is visible. Delivery is still unconfirmed.</span>
                ) : <ChatFooterError message={store.failedSend.delivery === 'unknown'
                  ? `Delivery is unconfirmed. ${store.failedSend.message}` : `Not sent. ${store.failedSend.message}`} />}
                {store.failedSend.delivery !== 'unknown' ? (
                  (store.failedSend.delivery !== 'refused' || composerDraft === '') && <>
                    <ChatFooterRemedy disabled={store.stalled || store.sending || !store.historyReady}
                      onClick={() => {
                        if (store.failedSend === null) return;
                        setComposerDraft('');
                        store.retrySend(store.failedSend.echo.id);
                      }}>
                      Try again
                    </ChatFooterRemedy>
                    <ChatFooterRemedy onClick={() => {
                      if (store.failedSend === null) return;
                      setComposerDraft(store.failedSend.echo.text);
                      registry.clearFailedSend(open.id, store.failedSend.echo.id);
                    }}>Edit</ChatFooterRemedy>
                  </>
                ) : (
                  <>
                    {store.matchingSendMessage ? (
                      <ChatFooterRemedy onClick={() => {
                        if (store.failedSend !== null) registry.clearFailedSend(open.id, store.failedSend.echo.id);
                      }}>I’ve checked</ChatFooterRemedy>
                    ) : <ChatFooterRemedy disabled={store.historyLoading} onClick={store.retryHistory}>
                      {store.historyLoading ? 'Checking…' : 'Check delivery'}
                    </ChatFooterRemedy>}
                    <ChatFooterRemedy disabled={store.stalled || store.sending || !store.historyReady}
                      onClick={() => setResendConfirmation(store.failedSend?.echo.id ?? null)}>
                      Send again…
                    </ChatFooterRemedy>
                  </>
                )}
              </ChatFooterNotice>
            )}
            <ConfirmDialog
              open={resendConfirmation !== null && store.failedSend?.echo.id === resendConfirmation}
              title="Send this message again?"
              description="It may already have arrived. Sending again can deliver the same request twice. Check the conversation for a reply first."
              confirmLabel="Send again"
              destructive={false}
              confirmState={store.stalled || store.sending || !store.historyReady ? 'blocked' : 'ready'}
              onConfirm={() => {
                if (resendConfirmation !== null) store.retrySend(resendConfirmation);
                setResendConfirmation(null);
              }}
              onCancel={() => setResendConfirmation(null)}
            />
            {store.actionError !== null && (
              <ChatFooterNotice><ChatFooterError message={store.actionError} /></ChatFooterNotice>
            )}
            {/* Not an `alert`: nothing just happened, the condition was already true when
                            this page opened. */}
            {store.blockedReason !== null && (
              <ChatFooterNotice>
                <ChatFooterError message={store.blockedReason} />
              </ChatFooterNotice>
            )}
            <ChatComposer
              /* Read at mount only, which is what makes it one-shot; the flag is dropped
                               when the drawer closes. */
              focusOnMount={composerFocusFor === open.id}
              draft={{ text: composerDraft, onChange: setComposerDraft }}
              disabled={store.sendBlocked || !store.historyReady}
              /* `delivered` is the one outcome that licenses forgetting the images; every
                               other one leaves the message with the reader. */
              onSend={(text) => {
                const sent = attachments.items;
                return store.send(open.id, text, sent).then((outcome) => {
                  if (outcome === 'delivered') attachments.clear();
                  return outcome;
                });
              }}
              allowEmptyText={attachments.items.length > 0}
              /* The queue lives inside the composer, above the field: these messages have
                               not reached the model, so they are not part of the conversation behind it. */
              drawer={(
                <>
                  <PendingQueue
                    entries={store.pendingQueue}
                    overflow={store.pendingQueueOverflow}
                    busy={store.sending}
                    onDelete={store.deleteQueuedEntry}
                    onSteer={store.steerQueuedEntry}
                  />
                  <PlannerAttachmentDrawer attachments={attachments} />
                </>
              )}
              /* Renders nothing until the harness has reported a usage frame. */
              sendAdornment={<ContextRing usage={store.contextUsage} />}
              /* `stopping` keeps Stop shown while the interrupt is in flight; `interrupt()`
                               already refuses a second one. */
              onStop={store.working || store.stopping ? store.interrupt : undefined}
              onNewConversation={startAnother}
              /* The kernel reads the selection when it hands a batch to codex, so a change
                               lands on the next turn not yet issued; a REFUSED turn is re-issued and
                               reads it again. */
              /* With `headerActions` unset Astryx does not render that row at all. */
              footerActions={(
                <HStack gap={1} align="center" className={footerStyles.group}>
                  <PlannerAttachButton
                    attachments={attachments}
                    support={{
                      available: store.attachmentsSupported,
                      reason: ATTACHED_WORKSPACE_REASON,
                    }}
                    disabled={store.sendBlocked || !store.historyReady}
                  />
                  <ModelPill
                    catalog={store.modelCatalog}
                    selection={store.model}
                    onChange={store.setModel}
                    isDisabled={!store.historyReady}
                  />
                </HStack>
              )}
            />
          </>
        )}
      >
        {/* Shown back because the composer clears its field on send; nothing here
                    claims they arrived. */}
        {draftOpen && (draft?.text == null
          ? <p>Nothing said yet. What you write starts the conversation.</p>
          : <>
            <p data-nc-turn="you">{draft.text}</p>
            {creating && <p role="status">Sending…</p>}
          </>)}
        {open !== null && (
          <>
            {store.hasEarlier && (
              <button type="button" disabled={store.loadingEarlier} onClick={store.loadEarlier}>
                {store.loadingEarlier ? 'Loading…' : 'Load earlier'}
              </button>
            )}
            {!store.historyReady && store.historyError === null && (
              <p role="status">Loading conversation…</p>
            )}
            {/* Keyed on the conversation: the drawer stays mounted across a switch, and
                          without the key `ChatThread`'s follow-the-newest refs and rail state
                          would be A's applied to B. */}
            {/* Not while the first page is unknown and there is nothing to show:
                          `ChatThread` draws its empty state (with the live dot) for an empty list.
                          `turnsOf` is the second arm so a reopen whose query was collected still
                          shows the remembered transcript. */}
            {(store.historyReady || store.turnsOf(open.id).length > 0) && (
              <ChatThread
                key={open.id}
                conversation={open}
                turns={store.turnsOf(open.id).filter((turn) => store.failedSend?.delivery !== 'refused'
                  || composerDraft === '' || turn.id !== store.failedSend.echo.id)}
                pending={store.pending.has(open.id)}
                cards={source.cards}
                stalled={store.stalled}
              />
            )}
          </>
        )}
      </Drawer>
    ),
  };
}

function TodayRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const workspace = useWorkspace(transport, unauthorized);
  const go = useGo();
  const preferences = useUiPreferences();
  const trackMutations = useTrackMutations(transport, unauthorized);
  const deletion = useDeleteConfirm((trackId, signal) => {
    const track = workspace.tracks.find((candidate) => candidate.id === trackId);
    if (track === undefined) throw new Error('This track is no longer available.');
    return trackMutations.remove(track.id, track.areaId, signal);
  });
  /* The launchpad resolve is a READ. `POST /api/today/launchpad/ensure` submits a
       harness start and waits on it, so it is never on the page-load path; `null`
       is the empty state, and every failure is rendered as one. */
  const launchpadQuery = useQuery(todayLaunchpadQueryOptions(transport, unauthorized));
  const launchpad = launchpadQuery.data;
  const launchpadTrackId = launchpad?.track_id ?? '';
  const [preparedLaunchpadTrackId, setPreparedLaunchpadTrackId] = useState<string | null>(null);
  const [conversationStartRequested, setConversationStartRequested] = useState(false);
  const conversationTrackId = launchpadTrackId || preparedLaunchpadTrackId || '';
  const launchpadEnsure = useTodayLaunchpadEnsureMutation(transport, unauthorized);
  /* Reset, `POST /api/today/launchpad/report/reset`; destructive, so it goes
       through `useDeleteConfirm`, keyed by the launchpad track id. */
  const reportReset = useTodayReportResetMutation(transport, unauthorized);
  const resetConfirm = useDeleteConfirm(() => reportReset.reset());
  /* The launchpad track's own conversations, by the same rule the track route uses. */
  const launchpadConversationsQuery = useQuery({
    ...trackConversationsQueryOptions(transport, conversationTrackId, unauthorized),
    /* No launchpad, no request: an ungated read would ask about a track named `''`. */
    enabled: conversationTrackId !== '',
  });
  const launchpadConversationMutations = useTrackConversationMutations(
    transport, conversationTrackId, unauthorized,
  );
  const launchpadRows = useMemo(
    () => (launchpadConversationsQuery.data ?? [])
      .map((row) => nameTodaySummaryConversation(conversationTrackId, row)),
    [conversationTrackId, launchpadConversationsQuery.data],
  );
  /* The launchpad is in the system area, which `GET /api/areas` filters out, so its
       overlays are read from the workspace-wide query directly (same key as
       `useWorkspace`, so the cache is shared). */
  const launchpadOverlaysQuery = useQuery({
    ...trackOverlaysQueryOptions(transport, unauthorized),
    enabled: conversationTrackId !== '',
  });
  const launchpadActivity = useMemo<TrackActivity>(
    () => trackActivityFrom(conversationTrackId, launchpadOverlaysQuery.data ?? []),
    [conversationTrackId, launchpadOverlaysQuery.data],
  );
  const chat = useConversationPanel(
    transport,
    unauthorized,
    {
      scopeId: conversationTrackId,
      rows: launchpadRows,
      cards: launchpadActivity.cards,
      /* The launchpad is a real track and these rows are its own; the store checks
             every row against this. */
      rememberOn: conversationTrackId,
      derivedCardId: (idempotencyKey) => trackConversationCardId(conversationTrackId, idempotencyKey),
      scopeOf: (conversationId) => {
        const row = launchpadRows.find((candidate) => candidate.id === conversationId);
        /* `id: row.trackId`, never `launchpadTrackId`: otherwise the `rememberOn`
                   comparison compares a value with itself. */
        return row === undefined ? null : {
          id: row.trackId, title: row.trackTitle, cardId: row.id, cardTitle: row.title,
          updatedAt: row.updatedAt, kind: row.kind, state: row.state,
        };
      },
      create: launchpadConversationMutations.create,
      refresh: launchpadConversationMutations.refresh,
    },
    /* Every row is on the launchpad, which is what this page is. */
    { showTrack: false },
  );

  const startTodayConversation = () => {
    if (launchpadEnsure.pending) return;
    if (conversationTrackId !== '') {
      /* `ensure` can materialise the launchpad and still fail its harness start; a
               retry then must not ask `ensure` to create it again. */
      launchpadEnsure.clearFailure();
      chat.startConversation();
      return;
    }
    void launchpadEnsure.ensure().then((prepared) => {
      /* The ensure response owns the track id, so the draft can be scoped
         without inventing one while the read-only resolve catches up. */
      setPreparedLaunchpadTrackId(prepared.track_id);
      setConversationStartRequested(true);
    }).catch(() => undefined);
  };

  useEffect(() => {
    if (!conversationStartRequested || conversationTrackId === '') return;
    chat.startConversation();
    setConversationStartRequested(false);
  }, [chat, conversationStartRequested, conversationTrackId]);

  const conversationList = launchpadQuery.isPending || launchpadQuery.isError
    /* The outer resolve is unknown or failed; neither means an empty list. */
    ? null
    : launchpadEnsure.pending
      ? <PanelEmpty>Preparing Today assistant…</PanelEmpty>
      : launchpadEnsure.failure !== null
        ? <ErrorBox
            message={`Today assistant could not be started: ${launchpadEnsure.failure.message}`}
            onRetry={startTodayConversation}
          />
        : conversationTrackId === ''
          ? <PanelEmpty>Start a conversation with Today.</PanelEmpty>
    : launchpadConversationsQuery.isPending
      /* Unknown is not empty: do not flash a false empty state while the first
         read is still on the wire. */
      ? null
      : launchpadConversationsQuery.isError
        ? <ErrorBox
            message={`Conversations are unavailable: ${launchpadConversationsQuery.error.message}`}
            onRetry={() => { void launchpadConversationsQuery.refetch(); }}
          />
        : chat.list;
  /* Gated on the server's own answer: when `report_has_noninitial_content` is
       false there is nothing to draw, so the page load stays at one request. */
  const launchpadHasContent = launchpad?.report_has_noninitial_content === true;
  const launchpadDetailQuery = useQuery({
    ...trackDetailQueryOptions(transport, launchpadTrackId, unauthorized),
    enabled: launchpadTrackId !== '' && launchpadHasContent,
  });
  const launchpadReport = useMemo(
    () => readTrackReport(launchpadDetailQuery.data?.cards ?? []),
    [launchpadDetailQuery.data],
  );
  /* Three states, not collapsed: `readTrackReport(...) === null` is true while in
       flight, on a failed read, and on an undecodable payload. */
  const launchpadDocument = launchpadDetailQuery.isError
    ? (
      <ErrorBox
        message={`Today's progress is unavailable: ${launchpadDetailQuery.error.message}`}
        onRetry={() => { void launchpadDetailQuery.refetch(); }}
      />
    )
    : launchpadDetailQuery.data === undefined
      // In flight. Nothing, not a placeholder: a skeleton that flashes on every load
      // is more motion than information.
      ? null
      : (
        <ReportDocument
          report={launchpadReport}
          /* The detail has arrived and the server says the report has content, so what
                       remains is a payload this build could not decode. */
          empty={<ReportEmpty
            lead="Today's report could not be read."
            hints={[
              'The server says it has been written, so this is a decoding problem, not an empty day.',
              'The report\'s payload is probably newer than this build.',
            ]}
          />}
        />
      );
  const workspaceError = workspace.areasError
    ?? workspace.trackErrorsByArea.values().next().value ?? null;
  if (workspace.areasLoading
    || (workspace.tracks.length === 0 && [...workspace.tracksLoadingByArea.values()].some(Boolean))) return null;
  return (
    <>
    {workspaceError !== null && <ErrorBox
      message={workspaceError.message}
      onRetry={() => {
        workspace.retryAreas(); workspace.retryOverlays();
        for (const area of workspace.areas) workspace.retryTracks(area.id);
      }}
    />}
    {workspace.overlaysError !== null && <ErrorBox message={`Track activity is unavailable: ${workspace.overlaysError.message}`} onRetry={workspace.retryOverlays} />}
    {deletion.feedback.error !== null && <div role="alert" data-nc-error-box="">
      <span>{deletion.feedback.error}</span>
      <button type="button" data-nc-action="tertiary" onClick={deletion.feedback.clear}>Dismiss</button>
    </div>}
    {resetConfirm.feedback.error !== null && <div role="alert" data-nc-error-box="">
      <span>{resetConfirm.feedback.error}</span>
      <button type="button" data-nc-action="tertiary" onClick={resetConfirm.feedback.clear}>Dismiss</button>
    </div>}
    <TodayPage
      activityAvailable={workspaceError === null && workspace.overlaysError === null
        && !workspace.areasLoading && !workspace.overlaysLoading
        && ![...workspace.tracksLoadingByArea.values()].some(Boolean)}
      tracks={workspace.tracks}
      areas={workspace.areas}
      // The row belongs to features/track and Today may not import a sibling domain,
      // so the composition layer injects it.
      renderTrackRow={(track, options) => (
        <TrackRow
          track={track}
          variant={options.variant}
          hourLabel={options.hourLabel}
          areaName={options.areaName}
          /* The same receipt key and comparison point as the rail, so a track reads as
                       unread on Today exactly when it does there. */
          unread={preferences.isUnread('track', track.id, track.activityAt ?? 0)}
          onOpen={(trackId) => go({ name: 'track', trackId })}
          /* The panel variant only: the main column's sections are the day's report,
                       not a place you edit from. */
          onDelete={options.variant === 'panel' ? deletion.request : undefined}
        />
      )}
      conversationList={conversationList}
      /* With no launchpad yet the slot stays visible; its press calls `ensure`
             explicitly, so the page load remains a pure read. */
      conversationAction={launchpadQuery.isPending || launchpadQuery.isError
        ? undefined
        : launchpadEnsure.pending
          ? undefined
          : conversationTrackId === '' || launchpadEnsure.failure !== null
            ? <PanelAction
                label="Start a conversation with Today"
                onClick={startTodayConversation}
              ><Icon name="plus" size="sm" /></PanelAction>
            : chat.action}
      /* Undefined while the resolve is in flight, `null` when the server says there
               is no launchpad yet. */
      launchpad={launchpadQuery.isError ? undefined : launchpad}
      launchpadDocument={launchpadDocument}
      launchpadError={launchpadQuery.isError
        ? <ErrorBox
          message={`Today's progress is unavailable: ${launchpadQuery.error.message}`}
          onRetry={() => { void launchpadQuery.refetch(); }}
        />
        : undefined}
      /* `TodayPage` decides whether to render it, on the same
               `report_has_noninitial_content` branch as the empty state. */
      documentAction={launchpadTrackId === '' ? undefined : (
        <button
          type="button"
          data-nc-action="destructive"
          disabled={resetConfirm.pending}
          aria-busy={resetConfirm.pending}
          onClick={() => resetConfirm.request(launchpadTrackId)}
        >
          {RESET_TODAY_REPORT_COPY.trigger}
        </button>
      )}
    />
    <ConfirmDialog
      open={deletion.open}
      title={DELETE_TRACK_COPY.title}
      description={DELETE_TRACK_COPY.description}
      confirmLabel={DELETE_TRACK_COPY.confirmLabel}
      confirmBusyLabel="Deleting…"
      confirmState={deletion.pending ? 'busy' : 'ready'}
      onConfirm={deletion.confirm}
      onCancel={deletion.cancel}
    />
    <ConfirmDialog
      open={resetConfirm.open}
      title={RESET_TODAY_REPORT_COPY.title}
      description={RESET_TODAY_REPORT_COPY.description}
      confirmLabel={RESET_TODAY_REPORT_COPY.confirmLabel}
      confirmBusyLabel="Resetting…"
      confirmState={resetConfirm.pending ? 'busy' : 'ready'}
      onConfirm={resetConfirm.confirm}
      onCancel={resetConfirm.cancel}
    />
    {chat.drawer}
    </>
  );
}

/**
 * `/recipes`. A 409 is the one status whose handling is "stay in edit mode and
 * keep every character", so it is decided here on `ApiError.failure.status`,
 * not on wording inside the feature.
 */
function RecipesRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const recipes = useTrackRecipes(transport, unauthorized);
  const mutations = useTrackRecipeMutations(transport, unauthorized);
  const { resolved } = useTheme();

  const write = async (draft: RecipeDraft, recipeId: string | null): Promise<RecipeWriteOutcome> => {
    try {
      const recipe = draft.if_revision === null || recipeId === null
        ? await mutations.create({ title: draft.title, body: draft.body })
        : await mutations.save(recipeId, {
          title: draft.title, body: draft.body, if_revision: draft.if_revision,
        });
      return { kind: 'saved', recipe };
    } catch (failure: unknown) {
      if (failure instanceof ApiError && failure.failure.kind === 'http' && failure.failure.status === 409) {
        return { kind: 'conflict' };
      }
      /* Reported verbatim: the kernel's 400 names the fence that would not parse. */
      return { kind: 'failed', message: failure instanceof Error ? failure.message : 'Could not save this recipe.' };
    }
  };

  return (
    <RecipesPage
      recipes={recipes.recipes}
      loaded={recipes.loaded}
      error={recipes.error}
      theme={resolved}
      onWrite={write}
      onDelete={mutations.remove}
    />
  );
}

/* Split in two: the hooks below need the track, which is only known after the
 * detail query resolves and three early returns have run. */
function TrackRoute({ transport, unauthorized, cardRuntime, recentFiles }: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  cardRuntime: CardRuntime;
  recentFiles: RecentFileHistory;
}) {
  const trackId = useRouteParam('/track/');
  const registry = useConversationRegistry();
  const detail = useQuery({
    ...trackDetailQueryOptions(transport, trackId ?? '', unauthorized),
    enabled: trackId !== undefined,
  });
  /* One `Track` per detail read, not per render: the body memoises on it. */
  const detailData = detail.data;
  const track = useMemo(
    () => detailData === undefined ? null : toTrack(detailData.track, trackActivityFrom(detailData.track.id, detailData.overlays)),
    [detailData],
  );
  /* The card Today asked for, if this track has it and it is a conversation card at
   * all — BOTH conversation markers, not just the planner one. A fail-safe with no
   * live producer. */
  const requestedCard = detail.data?.cards.find((card) => card.id === registry.requestedOpenId
    && card.kind === 'codex'
    && (isPlannerHarnessPayload(card.payload) || isAssistantHarnessPayload(card.payload)));
  const detailMatchesRoute = trackId !== undefined && detail.data?.track.id === trackId;
  useEffect(() => {
    if (registry.requestedOpenId === null || detail.isLoading || detail.isFetching) return;
    if (!detailMatchesRoute || requestedCard === undefined) registry.clearOpenRequest();
  }, [detail.isFetching, detail.isLoading, detailMatchesRoute, registry, requestedCard]);

  if (!detail.data || track === null) {
    if (detail.isLoading || detail.isFetching) return null;
    if (detail.error instanceof Error) return <ErrorBox message={detail.error.message} onRetry={() => { void detail.refetch(); }} />;
    return <PendingRoute label="Track" owner="features/track" missing />;
  }
  // `detail.data` can still be the previously-viewed track while this one
  // fetches; rendering it under this URL would show the wrong track.
  if (trackId !== undefined && detail.data.track.id !== trackId) return null;

  return (
    <TrackRouteBody
      key={track.id}
      transport={transport}
      unauthorized={unauthorized}
      track={track}
      canResumeTrack={detail.data.can_resume}
      cards={detail.data.cards}
      overlays={detail.data.overlays}
      cardRuntime={cardRuntime}
      recentFiles={recentFiles}
    />
  );
}

/** How a card without a goal is named in the Notifications aside: the planner by role, the rest by title, then kind. */
function notificationCardLabel(card: TrackDetailWire['cards'][number]): string {
  return card.kind === 'codex' && isPlannerHarnessPayload(card.payload)
    ? 'Planner'
    : card.kind === 'codex' && isAssistantHarnessPayload(card.payload)
      ? card.title ?? 'Assistant'
      : card.title ?? card.kind;
}

/**
 * The Notifications aside from `activity.items`, one row per card (`foldAttentionByCard`),
 * newest first. A card's row is named by the first line of its `payload.goal` (a worker card's
 * task, in words); a card with no goal (a terminal card, the planner) by `notificationCardLabel`.
 */
function attentionNotifications(
  items: TrackActivity['attentionItems'], cards: TrackDetailWire['cards'],
): readonly TrackInputNotification[] {
  return foldAttentionByCard(items).map((item): TrackInputNotification => {
    const card = item.cardId === null ? undefined : cards.find((candidate) => candidate.id === item.cardId);
    /* An item whose `card_id` names a card absent from `detail.cards` (deleted between
           ticks) is listed as `Card`; the next tick drops it. */
    const source = card !== undefined ? cardGoalTitle(card.payload) ?? notificationCardLabel(card)
      : item.origin === 'task' ? `Task ${item.id}`
        : item.origin === 'lifecycle' ? 'Track' : 'Card';
    const message = item.origin === 'card'
      ? (item.kind === 'input' ? 'Requires input to continue.' : 'Stopped with an error and needs attention.')
      : item.origin === 'session'
        ? (item.kind === 'input' ? 'Its session is waiting for input.' : 'Its session failed and needs attention.')
        : item.origin === 'task'
          ? (item.kind === 'input' ? 'The task is waiting for input.' : 'The task failed and needs attention.')
          : (item.kind === 'input' ? 'The track is waiting on you.' : 'The track failed and needs attention.');
    return {
      origin: item.origin,
      id: item.id,
      cardId: card === undefined ? null : card.id,
      source,
      message,
      state: item.kind === 'input' ? 'awaiting-input' : 'errored',
      updatedAt: item.atMs,
    };
  }).toSorted((left, right) => right.updatedAt - left.updatedAt);
}

function TrackRouteBody({
  transport, unauthorized, track, canResumeTrack, cards, overlays, cardRuntime, recentFiles,
}: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  track: Track;
  canResumeTrack: boolean;
  cards: TrackDetailWire['cards'];
  overlays: TrackDetailWire['overlays'];
  cardRuntime: CardRuntime;
  recentFiles: RecentFileHistory;
}) {
  useTrackViewState(track.id);
  // The same key and comparison point the rail uses: the overlay's completion
  // high-water mark, never the row's `updatedAt`.
  useReadReceipt('track', track.id, track.activityAt ?? 0);
  const trackMutations = useTrackMutations(transport, unauthorized);
  const conversationMutations = useTrackConversationMutations(transport, track.id, unauthorized);
  const openMobileSection = useOpenMobileSection();
  const mobileHeaderActionsHost = useMobileHeaderActionsHost();
  const mobileHeaderTitleHost = useMobileHeaderTitleHost();
  const mobileTrackChoices = useMobileTrackChoices();
  const go = useGo();
  const goSameTrack = useGoSameTrack();
  const fileNavigation = useTrackFileNavigation();
  const { openPanel, closePanel } = useTrackPanelNavigation();
  const requestedCardId = useRouteCardId();
  const rawRequestedFilePath = useRouteFilePath();
  const requestedFilePath = requestedCardId === null ? rawRequestedFilePath : null;
  const [recentFilePaths, setRecentFilePaths] = useState<readonly string[]>(
    () => recentFiles.read(track.id),
  );
  const fileReturnFocusRef = useRef<HTMLElement | null>(null);
  const previousFilePathRef = useRef(requestedFilePath);
  useEffect(() => {
    const previous = previousFilePathRef.current;
    previousFilePathRef.current = requestedFilePath;
    if (previous === null || requestedFilePath !== null) return;
    requestAnimationFrame(() => {
      const opener = fileReturnFocusRef.current;
      fileReturnFocusRef.current = null;
      const target = opener?.isConnected === true
        ? opener
        : document.querySelector<HTMLElement>('[data-nc-report]');
      target?.focus({ preventScroll: true });
    });
  }, [requestedFilePath]);
  /* The URL is turned into props here: `features-no-app` keeps `TrackPage` a pure
   * renderer. A live `?card=` wins over a hand-edited `?panel=`. */
  const routePanel = useRoutePanel();
  /* Whether `?panel=` describes anything at all on this viewport. */
  const compactViewport = useCompactViewport();
  /* `?from=` is a property of this visit, so every same-track move hands it back
   * explicitly (`go` clears what it is not given); crossing tracks drops it. */
  const routeFrom = useRouteFrom() ?? undefined;
  const cardRegistry = cardRuntime.registry;
  // The same predicate the planner entry resolves by, imported rather than copied.
  const plannerCard = cards.find((card) => card.kind === 'codex' && isPlannerHarnessPayload(card.payload));
  const registry = useConversationRegistry();
  /* The track's assistant conversations; the server's list predicate is
   * `role == Assistant`, so the planner row is injected below. */
  const conversationsQuery = useQuery(trackConversationsQueryOptions(transport, track.id, unauthorized));
  const assistantRows = useMemo(() => conversationsQuery.data ?? [], [conversationsQuery.data]);
  const trackTitle = trackDisplayTitle(track.title);
  // Planner is the one conversation projected from a card; its `state` is the
  // drawer's baseline, not an indicator — the row's dot reads `activity.cards`.
  const plannerRow = useMemo<Conversation | null>(() => plannerCard === undefined ? null : {
    id: plannerCard.id,
    trackId: track.id,
    trackTitle,
    title: plannerCard.title,
    kind: 'shared-spec',
    state: plannerCard.runtime?.status ?? null,
    updatedAt: plannerCard.runtime?.updated_at_ms ?? plannerCard.updated_at,
    // The receipt's comparison point; absent on a legacy snapshot.
    lastTurnCompletedAt: plannerCard.runtime?.last_turn_completed_ms ?? null,
  }, [plannerCard, track.id, trackTitle]);
  /* Redeeming the planner-open intent: `armed` is already "this track, this visit",
   * and the planner card's id exists only once the detail has landed. `disarm()`
   * before the open, unconditionally: an intent left armed on this entry would
   * fire on the next visit to it (Back reaches one). */
  const plannerOpenIntent = usePlannerOpenIntent(track.id);
  useEffect(() => {
    if (!plannerOpenIntent.armed) return;
    plannerOpenIntent.disarm();
    if (plannerCard === undefined) return;
    registry.requestOpen(plannerCard.id, { focusComposer: true });
  }, [registry, plannerCard, plannerOpenIntent]);
  /* Every row carries the track's title, unconditionally and before the planner
       row is considered. */
  const placedRows = useMemo(
    () => assistantRows.map((row) => ({ ...row, trackTitle })),
    [assistantRows, trackTitle],
  );
  const rows = useMemo<readonly Conversation[]>(
    () => plannerRow === null ? placedRows : [plannerRow, ...placedRows],
    [placedRows, plannerRow],
  );
  /* `'rows'` unconditionally: an empty list with a `+` over it is the whole feature. */
  const chat = useConversationPanel(
    transport,
    unauthorized,
    {
      scopeId: track.id,
      rows,
      /* The track's own overlay-derived verdicts (`toTrack(detail, detailActivity)`). */
      cards: track.cards,
      /* These rows are on a track the reader can be sent to, so Today may hold and
               open them; the store checks each row's `trackId` against this. */
      rememberOn: track.id,
      derivedCardId: (idempotencyKey) => trackConversationCardId(track.id, idempotencyKey),
      scopeOf: (conversationId) => {
        const row = rows.find(candidate => candidate.id === conversationId);
        /* `id: row.trackId`, never `track.id`: this is the line the `rememberOn`
         * comparison rests on, and written the other way it would be a tautology. */
        return row === undefined ? null : {
          id: row.trackId, title: trackTitle, cardId: row.id, cardTitle: row.title,
          updatedAt: row.updatedAt, kind: row.kind, state: row.state,
        };
      },
      create: conversationMutations.create,
      refresh: conversationMutations.refresh,
    },
    { showTrack: false },
  );
  /* The fallback clear: a request for a card this track has, while the list that
   * would open it could not be read. Not "the read failed", which would also
   * throw away an openable planner row. */
  useEffect(() => {
    const requestedOpenId = registry.requestedOpenId;
    if (requestedOpenId === null || !conversationsQuery.isError) return;
    if (rows.some((row) => row.id === requestedOpenId)) return;
    registry.clearOpenRequest();
  }, [conversationsQuery.isError, registry, rows]);
  /* The runtime half of the TASKS panel, keyed `['track-report', trackId]`: the key
   * `invalidation-plan` refreshes on every `task.*` event and `track.report_edited`. */
  /* Read before the query: the poll's interval depends on the rows this report can produce. */
  const report = useMemo(() => readTrackReport(cards), [cards]);
  const reportBlocks = report?.blocks ?? null;
  const verdictsQuery = useQuery(
    trackTaskVerdictsQueryOptions(transport, track.id, unauthorized, reportBlocks),
  );
  const verdicts = verdictsQuery.data;
  const { outline, tasks: joinedTasks } = useMemo(() => ({
    outline: deriveReportOutline(reportBlocks),
    /* The verdicts land a round-trip after the declarations; `undefined` renders
           the statusless list rather than a hole. */
    tasks: deriveReportTasks(reportBlocks, verdicts),
  }), [reportBlocks, verdicts]);
  /* The CARDS module lists cards that have a surface; unclaimed kinds stay listed,
   * and `originalIndex` is bound before the filter so the merge keeps wire order. */
  const panelCards = useMemo(() => {
    const { visible, unknown } = partitionTrackCards(cardRegistry, cards);
    return [...visible, ...unknown]
      .sort((left, right) => left.originalIndex - right.originalIndex)
      .map((slot) => slot.wire);
  }, [cardRegistry, cards]);
  const gridItems: readonly BoardHostItem[] = useMemo(() => {
    const { visible } = partitionTrackCards(cardRegistry, cards);
    return [...visible]
      .sort((left, right) => {
        const sort = left.wire.sort - right.wire.sort;
        return sort !== 0 ? sort : left.originalIndex - right.originalIndex;
      })
      .map((slot) => Object.freeze({
        card: slot.card,
        title: slot.wire.title ?? slot.wire.kind,
        originalIndex: slot.originalIndex,
        /* The kernel's bit, carried straight through, so the board and the CARDS panel
                   cannot disagree about which cards are the kernel's. */
        deletable: slot.wire.deletable,
        /* The kernel's verdict for the card, from the same overlay the CARDS row reads. */
        activity: cardActivityOf(track, slot.card.id),
      }));
  }, [cardRegistry, cards, track]);
  const inputNotifications = useMemo(
    () => attentionNotifications(track.attentionItems, cards),
    [cards, track.attentionItems],
  );
  /* Stable across renders that do not change the overlays, so a live table is
     not handed a new resolver identity on every keystroke elsewhere. */
  const resolveLiveTable = useCallback(
    (source: string) => liveTableOverlayPayload(track.id, overlays, source),
    [track.id, overlays],
  );
  /* One query per `chart.series` block, keyed by the block's rev; opening the
       report is what makes the kernel fetch the data. */
  const resolveSeries = useReportSeriesResolver(transport, track.id, reportBlocks, unauthorized);
  const conversationNotificationCardIds = useMemo(
    () => new Set(cards
      .filter((card) => card.kind === 'codex'
        && (isPlannerHarnessPayload(card.payload) || isAssistantHarnessPayload(card.payload)))
      .map((card) => card.id)),
    [cards],
  );
  /* The cards this route can open, asked of the registry through the list the board
   * draws. A worker card whose kind no entry claims is `unknown` and its `?card=`
   * is bounced; its task keeps its `workerCardId` regardless, because the TASKS
   * row looks its activity verdict up by that id. */
  const tasks = useCurrentTaskRows(track.id, joinedTasks);
  const openableCards = useMemo(() => new Set(gridItems.map((item) => item.card.id)), [gridItems]);
  const knownCard = requestedCardId !== null
    && gridItems.some((item) => item.card.id === requestedCardId);
  useEffect(() => {
    if (requestedCardId === null || knownCard) return;
    // Bouncing an unopenable `?card=` must drop only that parameter: the panel,
    // return surface and block anchor describe where the reader is.
    goSameTrack(track.id, { card: undefined }, { replace: true });
  }, [goSameTrack, knownCard, requestedCardId, track.id]);
  /* `?panel=` is a compact concept: above the breakpoint `TrackPage` would put
   * `inert` on the desktop panel. Corrected here with `replace`, and the
   * injection below is gated on the viewport too, because on a cold start this
   * effect has not run at first paint. */
  useEffect(() => {
    if (compactViewport || routePanel === null) return;
    goSameTrack(track.id, { panel: undefined }, { replace: true });
  }, [compactViewport, goSameTrack, routePanel, track.id]);
  /* One confirm for both delete gestures. Nothing navigates on success: the
   * `knownCard` effect bounces `?card=<deleted>` off the URL, and a `go()` here
   * would race it. */
  const cardDeletion = useDeleteConfirm(
    (cardId, signal) => trackMutations.removeCard(track.id, cardId, signal),
  );

  /* Adding a card: the menu is the registry's own list, so this route only decides
   * which endpoint a kind takes — `atomic` kinds have an endpoint named after the
   * kind, `generic` ones go through `POST /api/tracks/:id/cards` with the entry's
   * `claim` kind. An unhandled strategy throws rather than creating nothing. */
  const addMenuEntries = useMemo(() => cardAddMenuEntries(cardRegistry), [cardRegistry]);
  const listDirectory = useMemo(
    () => createDirectoryLister(transport, unauthorized),
    [transport, unauthorized],
  );
  const reportFiles = useMemo(
    () => createTrackWorkspaceFilesPort(transport, unauthorized, track.id),
    [track.id, transport, unauthorized],
  );
  const [cardDraft, setCardDraft] = useState<CardAddMenuEntry | null>(null);
  const [creatingCard, setCreatingCard] = useState(false);
  /* A failed create has to be sayable with no dialog on screen: a fieldless kind
   * never opens one, so the message gets a route-level surface. */
  const cardCreateFeedback = useOperationFeedback();
  const newCardFieldRef = useRef<HTMLInputElement | null>(null);
  /* A create that lands after the reader has left must not steer them: one
   * `AbortController` per attempt, aborted on unmount, read before the navigation
   * and every state write. */
  const activeCardCreate = useRef<AbortController | null>(null);
  useEffect(() => () => { activeCardCreate.current?.abort(); }, []);

  const createCardOfKind = async (entry: CardAddMenuEntry, values: NewCardValues) => {
    /* Empty is absent, not `""`: the kernel reads an empty `cwd` as "no directory
           given" but an empty `title` as a real, blank title. */
    const given = (key: string): string | undefined => {
      const value = (values[key] ?? '').trim();
      return value === '' ? undefined : value;
    };
    const title = given('title');
    /* Read at click time from `<html data-theme>`, not `useTheme()`: subscribing
           would remount any live terminal on every theme toggle. */
    const theme = readHostThemeRgb();
    if (entry.type === 'terminal') {
      return trackMutations.createTerminal(track.id, { theme, ...(title === undefined ? {} : { title }) });
    }
    if (entry.type === 'codex') {
      const cwd = given('cwd');
      return trackMutations.createCodex(track.id, {
        theme,
        ...(title === undefined ? {} : { title }),
        ...(cwd === undefined ? {} : { cwd }),
      });
    }
    const registered = cardRegistry.get(entry.type);
    const strategy = registered?.create;
    if (strategy?.mode !== 'generic' || registered?.claim?.mode !== 'exact') {
      throw new Error(`CardCreateUnsupported(${entry.type})`);
    }
    return trackMutations.createCard(track.id, {
      kind: registered.claim.kind,
      payload: strategy.buildPayload(values),
      ...(title === undefined ? {} : { title }),
    });
  };

  /* The create navigates to the new card, the same landing `onOpenCard` gives. */
  const submitNewCard = (entry: CardAddMenuEntry, values: NewCardValues) => {
    /* Only the newest gesture may own the landing: a superseded attempt is aborted so
           it neither steers nor clears a busy state that now belongs to the newer attempt. */
    activeCardCreate.current?.abort();
    const controller = new AbortController();
    activeCardCreate.current = controller;
    setCreatingCard(true);
    void cardCreateFeedback
      .run(
        createCardOfKind(entry, values).then((card) => {
          if (controller.signal.aborted) return;
          setCardDraft(null);
          goSameTrack(track.id, { card: card.id });
        }),
        `Could not create the ${entry.label} card.`,
        () => controller.signal.aborted,
      )
      .finally(() => {
        /* Only the attempt that still owns the busy state may clear it; both ways of
                   losing ownership abort, so `aborted` is the whole test. */
        if (controller.signal.aborted) return;
        activeCardCreate.current = null;
        setCreatingCard(false);
      });
  };

  /* A kind with nothing to ask is created on the spot; one with fields opens the form. */
  const pickCardKind = (entry: CardAddMenuEntry) => {
    cardCreateFeedback.clear();
    if (entry.fields.length === 0) submitNewCard(entry, {});
    else setCardDraft(entry);
  };
  const backlinksQuery = useQuery(trackBacklinksQueryOptions(transport, track.id, unauthorized));
  const backlinks = backlinksQuery.data;

  /* A `neige://wave/…` citation. Same track reveals immediately (so an unchanged
   * hash still flashes) then records the hash; the route body is keyed by track
   * id, so the document is preserved. */
  const arrivalAnchorId = useRouteHash();
  const openReportLink = (target: ReportLinkTarget) => {
    if (target.trackId === track.id) {
      if (target.blockId !== null) revealReportAnchor(target.blockId);
      // Same track: a move within the report, so the return surface survives and the
      // panel closes.
      go({ name: 'track', trackId: target.trackId, blockId: target.blockId ?? undefined, from: routeFrom });
      return;
    }
    // Another track: a real navigation. Nothing carries over.
    go({ name: 'track', trackId: target.trackId, blockId: target.blockId ?? undefined });
  };

  const rememberReportFile = (target: ReportFileLinkTarget) => {
    const relativePath = parseWorkspaceRelativeFilePath(target.path)?.path ?? null;
    if (relativePath === null) return null;
    setRecentFilePaths(recentFiles.record(track.id, relativePath));
    return relativePath;
  };

  const openReportFile = (target: ReportFileLinkTarget) => {
    const relativePath = parseWorkspaceRelativeFilePath(target.path)?.path ?? null;
    if (relativePath === null) return;
    if (requestedFilePath === null && document.activeElement instanceof HTMLElement) {
      fileReturnFocusRef.current = document.activeElement;
    }
    fileNavigation.openFile(track.id, relativePath);
  };

  /* A `neige://source/…` citation opens the source panel: route-local state, not
   * the URL, keyed to this body so leaving the track drops it. Phone and desktop
   * share the target. */
  const [sourceTarget, setSourceTarget] = useState<ReportSourceLinkTarget | null>(null);
  const sourceOpen = sourceTarget !== null;
  const openReportSource = (target: ReportSourceLinkTarget) => { setSourceTarget(target); };
  const closeReportSource = () => { setSourceTarget(null); };

  const closeBoard = () => {
    if (requestedFilePath !== null) {
      fileNavigation.closeFile(track.id);
      return;
    }
    go({ name: 'track', trackId: track.id, from: routeFrom }, { replace: true });
  };

  const boardOpen = knownCard || requestedFilePath !== null;

  const openReportAnchor = (blockId: string) => {
    revealReportAnchor(blockId);
    go({ name: 'track', trackId: track.id, blockId, from: routeFrom });
  };

  const taskFiles = useTaskArtifactFiles({ trackId: track.id, transport, unauthorized });
  const independentTask = useIndependentTaskLaunch({ trackId: track.id, cards, lifecycle: track.lifecycle, transport, unauthorized, onCreated: openReportAnchor });

  return (
    <>
    {independentTask.form}
    {taskFiles.dialog}
    <TrackStage>
    <TrackPage
      mobilePanelObscured={chat.isOpen || sourceOpen}
      mobileHeaderActionsHost={mobileHeaderActionsHost}
      mobileHeaderTitleHost={mobileHeaderTitleHost}
      mobileTitleReadView={mobileTrackChoices === null ? undefined : (controls) => <TrackSelector
        track={track} {...mobileTrackChoices(track.areaId)} controls={controls}
        onSelectTrack={(trackId) => go({ name: 'track', trackId, from: 'area' })} />}
      track={track}
      canResumeTrack={canResumeTrack}
      cards={panelCards}
      /* Derived from the report's own blocks, so the panel and the document
         cannot disagree about what tasks exist. */
      tasks={tasks}
      openableCards={openableCards}
      outlineItems={outline}
      /* Tasks and the mobile Outline share one anchor landing. The URL carries
         it too, so the reader can hand the destination to somebody else. */
      onOpenTask={openReportAnchor}
      onOpenOutline={openReportAnchor}
      onCreateTask={independentTask.open}
      cardsAction={<AddCardMenu entries={addMenuEntries} onSelect={pickCardKind} />}
      recentFiles={<RecentFiles
        paths={recentFilePaths}
        onOpen={(path) => openReportFile({ path })}
      />}
      onOpenCard={(cardId) => { go({ name: 'track', trackId: track.id, cardId, from: routeFrom }); }}
      onDeleteCard={cardDeletion.request}
      board={<>
        <CardGridOverlay
          open={knownCard}
          items={gridItems}
          host={cardRuntime.host}
          activeCardId={requestedCardId}
          onRemoveCard={cardDeletion.request}
          onClose={knownCard ? closeBoard : undefined}
        />
        {requestedFilePath !== null && (
          <ReportFileViewer
            key={requestedFilePath}
            path={requestedFilePath}
            files={reportFiles}
            fileRoot={track.cwd}
            wide={gridItems.length === 0}
            onClose={closeBoard}
            onFileOpened={(path) => { rememberReportFile({ path }); }}
            onOpenFileLink={openReportFile}
          />
        )}
      </>}
      onCloseBoard={boardOpen ? closeBoard : undefined}
      /* Gated on the viewport too: the effect above cannot have run yet on a desktop
               cold start, and one render "open" is one render with the desktop panel `inert`. */
      panel={renderedMobilePanel(routePanel, {
        compact: compactViewport,
        overlayOpen: requestedCardId !== null || rawRequestedFilePath !== null,
      })}
      onOpenPanel={(kind) => { openPanel(track.id, kind); }}
      onClosePanel={() => { closePanel(track.id); }}
      /* `?from=` is the whole memory of how the reader got here; absent means Pages. */
      mobileBackLabel={routeFrom === 'area' ? 'Tracks' : 'Pages'}
      onMobileBack={() => {
        if (routeFrom === 'area') openMobileSection({ kind: 'tracks', areaId: track.areaId });
        else openMobileSection({ kind: 'pages' });
      }}
      report={<ReportDocument
        report={report}
        /* `overlay.set` already invalidates this track's detail, so a plugin push
                   re-renders the block without the report being rewritten. */
        resolveLiveTable={resolveLiveTable}
        resolveSeries={resolveSeries}
        taskVerdicts={verdicts}
        taskRows={tasks}
        renderTaskExecution={(task, expanded) => <TaskRecovery
          key={`${track.id}:${task.key}`} trackId={track.id} taskKey={task.key} expanded={expanded}
          transport={transport} unauthorized={unauthorized} onViewArtifact={taskFiles.open}
          openableWorkerIds={openableCards}
          openWorker={(cardId) => { go({ name: 'track', trackId: track.id, cardId, from: routeFrom }); }}
        />}
        rail={<ReportOutline items={outline} />}
        backlinkCounts={backlinks === undefined ? undefined : backlinkCountsByBlock(backlinks.backlinks)}
        onOpenLink={openReportLink}
        onOpenFileLink={openReportFile}
        onOpenSourceLink={openReportSource}
        fileRoot={track.cwd}
        arrivalAnchorId={arrivalAnchorId}
        empty={<ReportEmpty
          lead="This track has not taken shape yet."
          hints={[
            'Say what you want in the conversation — the agent works it out with you and writes it up here.',
            'It stays with the track, so it is here the next time you open it.',
          ]}
        />}
      />}
      backlinks={backlinks !== undefined && backlinks.backlinks.length > 0
        ? (
          <ReportBacklinks
            trackId={track.id}
            backlinks={backlinks}
            onOpen={(trackId, blockId) => {
              // Same track keeps the return surface; a backlink into another track
              // is a departure and carries nothing.
              go({ name: 'track', trackId, blockId, from: trackId === track.id ? routeFrom : undefined });
            }}
          />
        )
        : undefined}
      conversationList={chat.list}
      conversationAction={chat.action}
      onStartConversation={chat.startConversation}
      conversationOpen={chat.isOpen}
      inputNotifications={inputNotifications}
      onOpenInputNotification={(cardId) => {
        /* No card to open (a lifecycle item, a task with no worker card yet): the
                   track itself is the destination. */
        if (cardId === null) {
          if (plannerCard !== undefined) registry.requestOpen(plannerCard.id, { focusComposer: true });
          else go({ name: 'track', trackId: track.id, from: routeFrom });
          return;
        }
        if (conversationNotificationCardIds.has(cardId)) {
          registry.requestOpen(cardId, { focusComposer: true });
          return;
        }
        if (!gridItems.some((item) => item.card.id === cardId)) return;
        chat.close();
        go({ name: 'track', trackId: track.id, cardId, from: routeFrom });
      }}
      onRenameTrack={(title) => trackMutations.patch(track.id, track.areaId, { title }).then(() => undefined)}
      onResumeTrack={() => trackMutations.patch(track.id, track.areaId, { lifecycle: 'working' }).then(() => undefined)}
      onDeleteTrack={(signal) => trackMutations.remove(track.id, track.areaId, signal).then(() => {
        if (signal.aborted) return;
        go({ name: 'today' });
      })}
    />
    </TrackStage>
    {/* Keyed by kind: switching kinds is a different form, and a shared mount
        would carry the previous kind's typed values into it. */}
    <Dialog
      open={cardDraft !== null}
      onClose={() => setCardDraft(null)}
      title={cardDraft === null ? '' : `New ${cardDraft.label} card`}
      initialFocusRef={newCardFieldRef}
    >
      {cardDraft !== null && (
        <NewCardForm
          key={cardDraft.type}
          entry={cardDraft}
          submitting={creatingCard}
          error={cardCreateFeedback.error}
          listDirectory={listDirectory}
          firstFieldRef={newCardFieldRef}
          onCancel={() => setCardDraft(null)}
          onSubmit={(values) => submitNewCard(cardDraft, values)}
        />
      )}
    </Dialog>
    <ConfirmDialog
      open={cardDeletion.open}
      title={DELETE_CARD_COPY.title}
      description={DELETE_CARD_COPY.description}
      confirmLabel={DELETE_CARD_COPY.confirmLabel}
      confirmBusyLabel="Deleting…"
      confirmState={cardDeletion.pending ? 'busy' : 'ready'}
      onConfirm={cardDeletion.confirm}
      onCancel={cardDeletion.cancel}
    />
    <OperationFeedback feedback={cardDeletion.feedback} />
    {/* Only while the dialog is closed: `NewCardForm` renders the same `error` inline. */}
    {cardDraft === null && <OperationFeedback feedback={cardCreateFeedback} />}
    {/* The source card is painted over the conversation's, which stays in the DOM
            underneath and so is `inert` for the duration. The wrapper is a static block
            so the drawer's absolute box still resolves against `.main`. */}
    <div data-nc-conversation-drawer-host="" inert={sourceOpen}>
      {chat.drawer}
    </div>
    <ReportSourceDrawer
      transport={transport}
      trackId={track.id}
      target={sourceOpen ? sourceTarget : null}
      unauthorized={unauthorized}
      onClose={closeReportSource}
    />
    </>
  );
}
