// Code-based TanStack Router setup.
//
// The whole tree is built inside a factory: `createRoute`/`createRouter` at
// module scope would be module runtime state, and injecting the transport and
// the QueryClient is what lets a test drive a real router without touching a
// module singleton.
//
// This module is also the composition point for route-owned feature surfaces.

import {
  createRootRoute, createRoute, createRouter, type AnyRoute,
} from '@tanstack/react-router';
import { useEffect, useMemo, useRef } from 'react';
import { useInfiniteQuery, useQuery, type QueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { folderConflictMessage } from '../../../../core/domain/area.ts';
import {
  isBlankForKernel, toTrack, trackActivityFrom, trackCreateKeyAction, trackDisplayTitle,
  type NewTrackBodyWithoutFirstMessage, type Track, type TrackDetailWire,
} from '../../../../core/domain/track.ts';
import type {
  BoardHostItem, CardAddMenuEntry, CardHost, CardRegistry,
} from '../../systems/cards/public.js';
import {
  cardAddMenuEntries, isAssistantHarnessPayload, isPlannerHarnessPayload, partitionTrackCards,
} from '../../systems/cards/public.js';
import { mintIdempotencyKey } from './idempotency-key.ts';
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
import { ReportBacklinks } from '../../features/report/backlinks/public.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
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
import {
  buildTranscript, conversationName, conversationNameFrom, CONVERSATION_STATE_SOURCE,
  conversationCreateFailure, CONVERSATION_TEXT_MAX, harnessItemToTurns, isOptimisticConversationTurn,
  mergeTranscript, reconcileOptimisticConversationTurns, reconcileUserEchoes, serverItemHighWater,
  trackConversationCardId,
  type Conversation, type ConversationKind, type ConversationMessage, type ConversationState,
  type ConversationTurn, type OptimisticConversationTurn, type TranscriptEntry,
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
  ApiError, folderConflictOf, harnessItemsQueryOptions,
  prefetchAreaList, plannerRunQueryOptions, todayLaunchpadQueryOptions,
  usePlannerMutations, useTodayLaunchpadEnsureMutation, useTodayReportResetMutation,
  useTrackConversationMutations, useTrackMutations, useTrackRecipeMutations, useTrackRecipes,
  useTrackTemplates, useWorkspace,
  trackBacklinksQueryOptions, trackConversationsQueryOptions, trackDetailQueryOptions,
  trackTaskVerdictsQueryOptions,
} from '../providers/queries.ts';
import { NewTrackForm, type NewTrackDraft } from '../../features/area/new-track/public.tsx';
import {
  RecipesPage, type RecipeDraft, type RecipeWriteOutcome,
} from '../../features/report/recipe/public.tsx';
import { useTheme } from '../theme/public.tsx';
import { AppShell, useOpenMobileSection } from '../shell/public.tsx';
import {
  ConversationProvider, useConversationRegistry,
  type ConversationDraft, type ConversationDraftId,
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
import { useCompactViewport } from '../../ui/viewport/public.ts';

export const APP_BASEPATH = '/next';

type ConversationStore = Readonly<{
  conversations: readonly Conversation[];
  /** Messages *and* the actions between them, in the order they happened. */
  turnsOf: (conversationId: string) => readonly TranscriptEntry[];
  pending: ReadonlySet<string>;
  working: boolean;
  stopping: boolean;
  sending: boolean;
  sendBlocked: boolean;
  historyReady: boolean;
  historyLoading: boolean;
  hasEarlier: boolean;
  loadingEarlier: boolean;
  historyError: string | null;
  actionError: string | null;
  send: (conversationId: string, text: string) => void;
  interrupt: () => void;
  retryHistory: () => void;
  loadEarlier: () => void;
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

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message !== '' ? error.message : fallback;
}

/**
 * Everything about the open conversation that does *not* come from its turns.
 *
 * Split out only so the row below can be derived twice from one expression —
 * see `useConversationStore` for why there are two.
 */
type ConversationFacts = Readonly<{
  cardId: string;
  trackId: string;
  trackTitle: string | undefined;
  cardTitle: string | null;
  kind: ConversationKind;
  state: ConversationState | null;
  working: boolean;
  /** The row's own time, used when no turn has supplied a later one. */
  fallbackUpdatedAt: number;
}>;

/**
 * The conversation row these turns describe.
 *
 * Three of its fields — the derived `title`, `updatedAt` and `turns` — are
 * statements *about the turns*, and are therefore only ever as true as the set
 * they are computed from. That is the whole reason this is a function of the
 * turns rather than a closure over the one list in scope: the drawer shows a
 * message the moment you press Enter, and "shown" and "happened" are not the
 * same claim (`useConversationStore`).
 */
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
    /* A server-listed conversation's state is the server's to report —
       `run_status_for` writes `turn_pending`, never `running`, for a headless
       harness, and everything outside the four live states arrives as `null`.
       The local phase still wins while a turn is in flight, because the list
       would otherwise sit on the state the last fetch happened to catch.
       Which kinds those are is a total table (`CONVERSATION_STATE_SOURCE`) and
       not a one-off kind test: this branch is silent, and a new kind
       falling into the `else` would swap the server's reading for an invented
       `'idle'` with nothing to notice it. */
    state: CONVERSATION_STATE_SOURCE[facts.kind] === 'server'
      ? (facts.working ? 'turn_pending' : facts.state)
      : (facts.working ? 'running' : 'idle'),
    updatedAt: turns.at(-1)?.atMs ?? facts.fallbackUpdatedAt,
    turns: turns.length,
  };
}

/** Project confirmed facts this tab learned back onto a server summary that
 * cannot carry a transcript-derived title or turn count. Server facts still
 * win when present, and time never moves backwards. */
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

/** A derived first-message title is stable for the conversation's lifetime and
 * may be shown after close. Counts and activity time are snapshots, so only the
 * open row may claim those values in the product list. */
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
  const mutations = usePlannerMutations(transport, cardId, unauthorized);
  const [echoes, setEchoes] = useState<readonly OptimisticConversationTurn[]>([]);
  /**
   * The echo whose `POST /planner/input` has not been answered yet, if any.
   *
   * One id and not a set, and that is a claim about reachability rather than
   * about this line: a second unanswered echo would make `confirmedEchoes`
   * report the first one as confirmed, which is exactly the false fact the
   * registry must never be told (`durableConversation`).
   *
   * `sendingRef` and `activeSend` govern this store's state; the provider's
   * per-conversation send lease governs the lifetime they cannot see. Leaving
   * and remounting the same conversation therefore cannot start a second send
   * while the first request is unanswered, while a different conversation may
   * still send independently. The settle handlers below touch local state only
   * while they remain active, but always release the provider lease they own.
   */
  const [unconfirmedEchoId, setUnconfirmedEchoId] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [interruptPending, setInterruptPending] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const sendingRef = useRef(false);
  /**
   * The send whose settling is still allowed to speak for this store.
   *
   * A request that is no longer this one has nothing true to say about
   * `sending`, `sendingRef` or `actionError`: those describe the conversation
   * the reader is in now, and it is not the conversation that request was sent
   * to. (Its *own* result is still written through — see `send`.)
   */
  const activeSend = useRef<{ cardId: string; echoId: string } | null>(null);
  const items = useMemo(() => (history.data?.pages ?? []).flat(), [history.data]);
  /* A remembered transcript is the reopen fallback while the first page is
     unknown — initial pending, a failed read, or a query that was collected
     after the drawer closed. Once any query data exists, the server wins even
     when its answer is genuinely empty. */
  const serverEntries = useMemo(
    () => history.data === undefined
      ? registry.turnsOf(cardId).filter((entry) => !isOptimisticConversationTurn(entry))
      : buildTranscript(items),
    [cardId, history.data, items, registry],
  );
  const serverTurns = useMemo(
    () => history.data === undefined
      ? serverEntries.filter((entry): entry is ConversationMessage => entry.author !== 'activity')
      : [...items].sort((left, right) => left.id - right.id).flatMap(harnessItemToTurns),
    [history.data, items, serverEntries],
  );
  useEffect(() => {
    setEchoes([]);
    setUnconfirmedEchoId(null);
    setActionError(null);
    /* The send in flight, if any, belongs to the conversation being left: it
       stops being the active one here, and stops being allowed to write the
       state below. Its own answer is still delivered (`send`). */
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
  /*
   * The same turns, minus the one nobody has agreed to yet.
   *
   * `reconcileUserEchoes` drops an echo once the server sends the message back,
   * so every echo still standing is either in flight or already confirmed by
   * the 200 its own POST returned; this removes the first kind. `serverTurns`
   * needs no filter — it *is* the server's account.
   */
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
  /* An echo is a message you already sent, so it belongs after everything the
     server has confirmed. Keep `buildTranscript`'s positional pairing intact:
     a completed action retains the started row's place even when its end time
     is later than an interleaved message. A completed tail thought stops being
     the tail as soon as the user speaks again. */
  /*
   * ── The create placeholder (#1449) ────────────────────────────────────────
   *
   * Read, retired, and rendered — and nothing else touches it. It is not in
   * `echoes`, `turns`, `confirmedTurns` or `confirmedTranscript`, so no send
   * reconciliation can spend a server row on it, no conversation metadata
   * counts it, it is never written to the registry's turn list, and it cannot
   * reach `hasUnreconciledSend`.
   */
  const createEcho = registry.createEchoOf(cardId);
  /*
   * Whether the server has this sentence *now*, decided at render.
   *
   * At render and not in the effect below, because an effect runs after the
   * commit: the page that brings the sentence back would paint once with the
   * placeholder and the persisted row side by side — the reader's words
   * twice — and only then settle. This decides what is shown; the effect only
   * writes the fact down.
   */
  const createEchoShown = useMemo(
    () => createEcho !== null && serverTurns.some((turn) => turn.author === 'you'
      && reconcileUserEchoes([turn], [createEchoLine(createEcho)]).length === 0),
    [createEcho, serverTurns],
  );
  /* `getNextPageParam` (`app/providers/queries.ts`) reports this off the last
     page fetched, which is the oldest one loaded. */
  const hasEarlierPage = history.hasNextPage;
  const retireCreateEcho = registry.retireCreateEcho;
  useEffect(() => {
    if (createEcho === null) return;
    /*
     * Written down once, and never asked again.
     *
     * One way, and that is the point rather than an optimisation. A transcript
     * page is a window — the reader's query asks for the newest rows and every
     * send invalidates it — so "is my sentence in what is loaded?" is a fact
     * about the last fetch, not about the conversation. Recomputed as the
     * standing answer it says yes, then no once the agent has written a
     * pageful past it, and no again the moment another client resets the card
     * and the first page comes back empty. A line that un-retires reappears at
     * the head of a thread that has long moved on.
     *
     * The criterion is this sentence rather than "any user turn": if the agent
     * never echoes this one — the create's message stranded in the pending
     * queue, which #1449 does not fix — the reader keeps seeing what they
     * typed instead of watching it vanish behind a later message.
     * `reconcileUserEchoes` is the matcher the send path already uses, called
     * with one echo, so this is a scan and not a pairing.
     */
    if (createEchoShown || hasEarlierPage) retireCreateEcho(cardId);
  }, [cardId, createEcho, createEchoShown, hasEarlierPage, retireCreateEcho]);
  const transcript = useMemo(
    () => {
      const merged = mergeTranscript(serverEntries, echoes);
      if (createEcho === null || createEchoShown) return merged;
      /* Borrowing the time of the entry it precedes — see `createEchoLine`. */
      return [createEchoLine(createEcho, merged[0]?.atMs ?? 0), ...merged];
    },
    [createEcho, createEchoShown, echoes, serverEntries],
  );
  const confirmedTranscript = useMemo(
    () => mergeTranscript(serverEntries, confirmedEchoes), [confirmedEchoes, serverEntries],
  );
  const phase = run.data?.phase ?? null;
  const working = phase === 'issuing_turn' || phase === 'turn_running';
  const stopping = phase === 'issuing_interrupt' || interruptPending;
  const facts = useMemo<ConversationFacts | null>(() => trackId === undefined ? null : {
    cardId, trackId, trackTitle, cardTitle: cardTitle ?? null, kind: scopeKind,
    state: scopeState, working, fallbackUpdatedAt: scopeUpdatedAt ?? 0,
  }, [cardId, cardTitle, scopeKind, scopeState, scopeUpdatedAt, trackId, trackTitle, working]);
  /**
   * What the reader is looking at: every turn, echoes included.
   *
   * This is the optimistic row. Pressing Enter has to put the message on the
   * screen and name the drawer after it immediately — that is the whole point
   * of an echo — and this route replaces the open row in its own list with this
   * value so the panel behind the drawer agrees with the drawer.
   */
  const conversation = useMemo(
    () => facts === null ? null : describeConversation(facts, turns), [facts, turns],
  );
  /**
   * What the tab will still believe once the drawer is gone: confirmed turns
   * only.
   *
   * The registry is not a view, it is a **memory** — nothing refreshes it, it
   * has no `forget`, and it is kept for the life of the tab. So the one thing
   * that may never enter it is a fact that is not yet a fact.
   *
   * Who still reads it, stated plainly because #1341 changed the answer and a
   * stale version of this note would be a lie about coverage. Today used to
   * render this memory as its conversation list, and no route does so now.
   * What is left are two live readers,
   * and they both read the **turns**: optimistic reconciliation uses their
   * persisted provenance, and the drawer falls back to them while history is
   * unknown. The remembered `title` and `updatedAt`
   * currently have no reader at all — they are kept because the cross-track
   * conversation card (#1341, separate issue) is a list reader coming back, and
   * because the rule below is about what may be *written*, which is cheaper to
   * keep true than to re-derive. `track-conversation.test.tsx`'s
   * `registry write-through` block asks all of this of the registry directly.
   *
   * The shape that got in: an assistant card is minted `title: null` and the
   * only name it ever has is the one derived from its first message, and an
   * echo's `atMs` is `Date.now()` on the *browser's* clock. Send the first
   * message on a fresh conversation, close the drawer while the POST is still
   * in flight, and let the POST fail — the drawer's `catch` drops the echo, but
   * `scope` is null by then, so the effect below no longer runs and nothing
   * revisits the entry. Today was left naming a conversation after a message
   * that never left the browser, and holding it at the top of the list on a
   * clock reading nobody else shares.
   *
   * Deriving the remembered row from the confirmed turns closes it at the
   * source rather than by repair: the false value is never written, so there is
   * nothing for the failure path to undo, and the rule holds for any future
   * writer of this entry rather than for the one that was found.
   */
  const durableConversation = useMemo(
    () => facts === null ? null : describeConversation(facts, confirmedTurns),
    [confirmedTurns, facts],
  );
  useEffect(() => {
    if (durableConversation === null) return;
    /* Server rows enter the registry only under the Track that supplied them. */
    if (durableConversation.trackId !== rememberOn) return;
    /* Remember the transcript so reopening the conversation preserves its
       activity lines and looks identical to the route the user just left — the
       confirmed one, for the reason `durableConversation` gives: a message that
       may still fail is not part of what this conversation *is*. */
    registry.remember(durableConversation, confirmedTranscript);
  }, [confirmedTranscript, durableConversation, registry, rememberOn]);
  useEffect(() => {
    /*
     * A `'rows'` route that named a track remembers **every** row it lists.
     *
     * Not only the open one, and not only on open. This gives every route row a
     * stable place for confirmed facts learned from its transcript, so closing
     * the drawer can project those facts back onto the server summary below.
     *
     * `rememberOn` is compared against each row rather than merely consulted: a
     * row belonging to some other Track must not write facts into this route's
     * registry scope. The comparison is the defence, held here rather than in a
     * renderer.
     *
     * Turns are carried over from whatever the registry already holds, never
     * reset: the open row is remembered with its full transcript by the effect
     * above, and writing `[]` here would erase it on the next render.
     */
    for (const row of serverRows) {
      if (row.trackId !== rememberOn) continue;
      /* The open row belongs to the effect above, which knows its transcript
         and its live name. Writing the plain row over that here would undo it
         on every render, and the two effects would then take turns rewriting
         one entry for as long as the drawer stayed open. */
      if (row.id === conversation?.id) continue;
      /* A turn count this tab really read is not unread by a list that does not
         send one. The server will not count turns (`TrackConversationSummary`),
         so a row always arrives with `turns` absent; writing that over an entry
         the drawer counted would make the registry forget a confirmed turn the
         moment its Track was refreshed. The transcript is carried over for the
         same reason and by the same rule.

         And so is the **name**, which is that rule a third time and was the one
         omission: an assistant card is minted `title: None`
         (`track_conversations.rs`) and nothing backfills it, so `row.title` is
         permanently null on the wire. The name a reader sees is derived by the
         effect above from the conversation's first message. The moment the
         drawer closes — or opens on another row — this effect stops skipping
         that row, and a plain `{...row}` would put the null back: the route row
         would fall to the bare kind label `Assistant`. Only
         the absent direction is carried — a title the server does send in a
         future backfill is the server's to change and wins, exactly as `turns`
         does. */
      /* `updatedAt` never goes backwards, which is the same rule once more.
         A row's time is whatever column produced it — the listed rows read
         `COALESCE(worker_sessions.updated_at_ms, cards.updated_at)`, the
         injected planner row reads the card's `updated_at`, and neither moves
         when a turn is added to a conversation the drawer is reading. The
         drawer *does* know that time (`turns.at(-1)?.atMs`) and wrote it here.
         Taking the later of the two keeps the registry's memory monotonic. The
         product list deliberately does not project this stale snapshot; only
         the open row claims a current activity time. */
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
  /* The open row is replaced in place by the live one: same id, but with the
     turns and the name this route can only know from the transcript it is
     already reading (§7 — the server has no title to send). */
  const conversations = conversation === null
    ? listedConversations
    : listedConversations.map((row) => row.id === conversation.id ? conversation : row);

  const send = (_conversationId: string, text: string) => {
    if (sendingRef.current || !registry.tryBeginSend(cardId)) return;
    sendingRef.current = true;
    setSending(true);
    setActionError(null);
    const echo: OptimisticConversationTurn = {
      id: `echo-${mintIdempotencyKey()}`, author: 'you' as const, text, atMs: Date.now(),
      serverHighWaterBefore: serverItemHighWater(items),
    };
    const sentTo = cardId;
    activeSend.current = { cardId: sentTo, echoId: echo.id };
    /* Still ours to answer for. False from the moment the reader moved to
       another conversation (the `cardId` effect) or started a later send. */
    const stillActive = () => activeSend.current?.echoId === echo.id;
    let sendFailure: string | null = null;
    setEchoes((current) => [...current, echo]);
    setUnconfirmedEchoId(echo.id);
    void mutations.send(text).then(() => {
      setUnconfirmedEchoId((current) => current === echo.id ? null : current);
      /*
       * The answer can outlive the drawer, and the effects above cannot.
       *
       * Closing the drawer takes `scope` to null, so `durableConversation`
       * becomes null and this store stops writing to the registry — including
       * for a turn that lands a moment later and *is* now a fact. Nothing else
       * would ever supply it: an assistant row is `title: null` on the wire for
       * good, so the name would be lost for the life of the tab rather than
       * merely delayed. So the confirmation is written straight through — for
       * the conversation it was *sent to*, which is not necessarily the one
       * open now.
       *
       * Through `updateExisting`, not `remember`, because both halves of this
       * write have to happen at the moment of the write rather than at the
       * moment of the send. The POST acknowledgement starts two background
       * refreshes (`usePlannerMutations`), and those reads — or an event that
       * arrived while the POST was in flight — can put a newer transcript,
       * turn count or state in this entry first. Merging into
       * `registry.conversations` and `registry.turnsOf` as captured here would
       * put the pre-send entry back and drop what just arrived. The
       * "only into an entry that already exists" check is the same story: a
       * decision made off a captured list is a decision made about a list that
       * may no longer be the one being written to.
       *
       * Only the absent direction of the name, exactly as the batch remember
       * below does it — a title the server sends is the server's.
       */
      registry.updateExisting(sentTo, ({ conversation: known, turns: knownTurns }) => {
        /*
         * The refresh may already have brought this very message back.
         *
         * Every optimistic turn carries the server item high-water from before
         * its own send. Reconcile all of them together, oldest first, so one new
         * server row can confirm only one echo — including echoes minted by an
         * older store instance. Provenance, rather than this store's local id
         * set, is what survives a route remount.
         */
        const remembered = knownTurns.filter(isOptimisticConversationTurn);
        const optimistic = remembered.some((turn) => turn.id === echo.id)
          ? remembered
          : [...remembered, echo].toSorted((left, right) => left.atMs - right.atMs);
        const serverMessages = knownTurns.filter((turn): turn is ConversationMessage =>
          turn.author !== 'activity' && !isOptimisticConversationTurn(turn));
        const unresolved = reconcileOptimisticConversationTurns(serverMessages, optimistic);
        const unresolvedIds = new Set(unresolved.map((turn) => turn.id));
        const recorded = !unresolvedIds.has(echo.id);
        const nextTurns = knownTurns.filter((turn) =>
          !isOptimisticConversationTurn(turn) || unresolvedIds.has(turn.id));
        if (!recorded && !nextTurns.some((turn) => turn.id === echo.id)) nextTurns.push(echo);
        return {
          conversation: {
            ...known,
            title: known.title ?? conversationNameFrom(text),
            updatedAt: Math.max(known.updatedAt, echo.atMs),
            turns: nextTurns.filter((turn) => turn.author !== 'activity').length,
          },
          turns: nextTurns,
        };
      });
    }).catch((error: unknown) => {
      sendFailure = errorMessage(error, 'Could not send the message.');
      /* A failure belongs to the conversation that failed. Reported on another
         one it is a sentence under a composer the reader never sent from, and
         dropping the echo there would be dropping someone else's. The provider
         still records this failure below for a remount of the owning card. */
      if (!stillActive()) return;
      setEchoes((current) => current.filter((turn) => turn.id !== echo.id));
      setActionError(sendFailure);
    }).finally(() => {
      setUnconfirmedEchoId((current) => current === echo.id ? null : current);
      registry.finishSend(sentTo, sendFailure);
      /* Re-opening the composer is a statement about the send in flight *now*.
         Made unconditionally, this is what let a second unanswered echo exist:
         the request left behind by a conversation switch cleared the flag of a
         send that had not been answered yet. See `unconfirmedEchoId`. */
      if (!stillActive()) return;
      activeSend.current = null;
      sendingRef.current = false;
      setSending(false);
    });
  };

  /*
   * Stopping says so by *stopping*, not by a line of text.
   *
   * A successful interrupt used to set `Turn stopped` under the composer. Two
   * things already carry that fact at the moment it becomes true: the Stop
   * button turns back into Send, and the activity line stops advancing. A
   * sentence saying it a third time is the kind of confirmation that reads as
   * chrome — and unlike every other state on this surface it had no way to
   * expire, so it sat under the box until the next send. A state that
   * only the *next* action can clear is not a status, it is a residue.
   *
   * Failure still speaks (`actionError`): that one is not visible anywhere else.
   */
  const interrupt = () => {
    if (!working || stopping) return;
    setInterruptPending(true);
    setActionError(null);
    void mutations.interrupt().catch((error: unknown) => {
      setActionError(errorMessage(error, 'Could not stop the turn.'));
    }).finally(() => setInterruptPending(false));
  };

  const sendingAcrossMounts = cardId !== '' && registry.pendingSendIds.has(cardId);
  const hasUnreconciledSend = echoes.length > 0
    || registry.turnsOf(cardId).some(isOptimisticConversationTurn);
  const sendBlocked = sending || sendingAcrossMounts || hasUnreconciledSend;
  return {
    conversations,
    turnsOf: (conversationId) => conversation?.id === conversationId
      ? transcript
      : registry.turnsOf(conversationId),
    pending: pendingConversationIds(conversation, working, sending || sendingAcrossMounts),
    working,
    stopping,
    sending: sending || sendingAcrossMounts,
    sendBlocked,
    historyReady: history.data !== undefined,
    historyLoading: history.data === undefined && history.isFetching,
    hasEarlier: history.hasNextPage,
    loadingEarlier: history.isFetchingNextPage,
    historyError: history.error instanceof Error ? history.error.message : null,
    actionError: actionError ?? registry.sendErrors[cardId] ?? null,
    send,
    interrupt,
    retryHistory: () => { void history.refetch().catch(() => undefined); },
    loadEarlier: () => { void history.fetchNextPage().catch(() => undefined); },
  };
}

/**
 * The one conversation whose transcript is being read.
 *
 * `id` is the Track the card hangs off; `title` is that Track's title when the
 * surface knows it. `kind` carries what the list row already knew, so opening
 * an assistant row cannot make it read as a planner one. `state` carries the row's server state as
 * the *baseline*; the open row is the only one this route can watch live, so it
 * — and only it — also picks up the local phase (`turn_pending` while a turn is
 * in flight) and the name derived from its first message, which is why the open
 * row can show a name and a dot the closed rows cannot (§7).
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
    /** See `ConversationRouteIntent`: the Track these rows may be sent to. */
    rememberOn: string;
    scopeOf: (conversationId: string) => PlannerConversationScope | null;
    /** The id the card minted under this key will have, derived before the POST. */
    derivedCardId: (idempotencyKey: string) => string;
    create: (text: string, idempotencyKey: string) => Promise<Conversation>;
    refresh: () => Promise<readonly Conversation[]>;
  }>;

/**
 * The identity of a create's placeholder line in the transcript.
 *
 * Fixed, because there is at most one per card. It is read by React's list
 * reconciliation and by `exchangesOf`, which makes it the identity and the
 * label of the exchange the rail draws for this line.
 */
const CREATE_ECHO_ID = 'create-echo';

/**
 * The sentence a create delivered, as one transcript line pinned at the head.
 *
 * Why it exists: a transcript is read from one persisted table
 * (`crates/calm-truth/src/db/sqlite/read.rs`), and a row lands in that table
 * only when codex echoes the turn back
 * (`crates/calm-server/src/harness/run_loop.rs`). The create POST delivering
 * the message is therefore not the message being *readable*: between the 201
 * and codex's echo — seconds, or unbounded when the agent is down — the new
 * card's item read answers `[]`. Without this the reader's own first sentence
 * was on no surface at all, and the thread painted its empty state beside a
 * live `Working` dot.
 *
 * Why it is a plain line and not an `OptimisticConversationTurn`: see
 * `CreateEchoSlot` in `app/conversations`. It carries no provenance because it
 * is never reconciled.
 *
 * **`atMs` is borrowed, not invented.** It is read: the thread stamps a time
 * wherever two consecutive entries are `CONVERSATION_GAP_MS` apart
 * (`opensAfterGap`), so a made-up `0` put a ten-minute separator between this
 * line and the very next thing on screen — a gap the reader never took. The
 * placeholder takes the time of the entry it sits in front of, so the distance
 * it introduces is zero and the separator it introduces is none. That is a
 * server timestamp copied, not a clock read: nothing here compares the
 * browser's clock with the kernel's, which is the mistake that produced a
 * different misordering two rounds ago. With nothing to sit in front of there
 * is nothing to be apart from, and the value is unobservable.
 *
 * **The hole this leaves, stated rather than papered over** (#1475): the slot
 * lives in this tab's memory. Reload before codex echoes and the thread is
 * empty again; a second device never sees it. Making the sentence readable
 * from `GET /api/cards/{id}/harness/items` is a persistence-boundary change
 * with its own review surface and is deliberately not this change.
 *
 * **KNOWN GAP — the retirement criterion is `userTextMatchesEcho`, which is
 * wider than equality.** It also matches a persisted row that *starts with*
 * this sentence followed by a newline. So a later, longer, genuinely different
 * message whose first line happens to repeat these words retires the slot —
 * and when the create's own sentence was stranded and never delivered, that is
 * the reader's words disappearing from the head of the thread for good. Not
 * narrowed here: the matcher is the send path's, shared on purpose, and
 * changing it is a change to the send path.
 *
 * **KNOWN GAP — a full page retires the slot.** When the oldest loaded page
 * came back full, the sentence is removed as soon as that page lands.
 * Reachable through the redemption re-armed after a failed landing
 * (`usePlannerOpenIntent`), which can mint a slot on a planner card that has
 * been talking for days.
 *
 * **KNOWN GAP — a reader who has paged back gets the opposite.** Measured on
 * a 350-row card: after the first page the guard is true at 300 rows; after
 * `Load earlier` it is false at 350 rows, and a slot minted then sits at the
 * head of all 350.
 *
 * **KNOWN GAP — a slot that never saw its row outlives the tab.** Nothing
 * retires a placeholder whose sentence the agent never echoes, so if another
 * client empties the conversation
 * (`POST /api/cards/{id}/planner/reset`) the reader can open an emptied thread
 * and be shown a line from a session that no longer exists. It is one stale
 * line and nothing more: it is not in `turns`, `confirmedTurns`, `echoes` or
 * `hasUnreconciledSend`, so it does not name the conversation, orders nothing,
 * consumes no server row and cannot shut the composer.
 *
 * A visit-scoped bound was tried and withdrawn: it did not close this — the
 * store's effects see `cardId` change only while the hook stays mounted, so
 * navigating away retired nothing — and it cost the feature outright, because
 * closing and reopening the drawer erased the sentence. If this is to be
 * closed, the signal is the `harness.transcript.cleared` event the frontend
 * already consumes (`fe/core/events/invalidation-plan.ts`), which names the
 * hazard itself. It is **not** the identity of the runtime the slot was minted
 * under: a repoint changes the runtime too, and the kernel work in flight for
 * #1449 exists precisely to carry an undelivered sentence across a repoint, so
 * retiring on a runtime change would hide it at the moment the kernel saved
 * it.
 *
 * **KNOWN GAP — the placeholder takes the first exchange's rail dot.**
 * `exchangesOf` (`features/chat/thread`) opens an exchange at a `you` turn
 * whose predecessor is not one. With the placeholder at index 0 and a server
 * `you` turn at index 1, that message stops opening an exchange and the rail's
 * first dot is labelled with the placeholder's text. Registered rather than
 * fixed: excluding it from `exchangesOf` means teaching the rail about a kind
 * of line that exists for one feature.
 */
function createEchoLine(text: string, atMs = 0): ConversationTurn {
  return { id: CREATE_ECHO_ID, author: 'you', text, atMs };
}

/** Which row is open, or the draft that has not become a row yet. */
type OpenTarget = Readonly<{ kind: 'row'; id: string } | { kind: 'draft' }>;

/** What a caller may change without touching the draft's identity. `key` and
 *  `sentText` are deliberately absent: they move together or not at all, which
 *  is why `rekeyDraft` and `markDraftSent` are the only doors to them. */
type DraftEdit = Partial<Pick<ConversationDraft, 'text' | 'creating' | 'error' | 'remedy'>>;

/**
 * The card runtime, created once at boot and injected like every other
 * instance-owned dependency. The track route mounts visible cards into the
 * grid overlay through `host`.
 */
export type CardRuntime = Readonly<{ registry: CardRegistry; host: CardHost }>;

export type AppRouterDeps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  client: QueryClient;
  onSignOut: () => void;
  cards: CardRuntime;
  recentFiles?: RecentFileHistory;
}>;

/** The component every settings route uses; see `settingsRoute` below. */
function renderNothing(): null { return null; }

export function createRouteTree(deps: AppRouterDeps): AnyRoute {
  const { transport, unauthorized, client, onSignOut, cards } = deps;
  const recentFiles = deps.recentFiles ?? createRecentFileHistory();
  const rootRoute = createRootRoute({ component: () => <ShellRoute transport={transport} unauthorized={unauthorized} onSignOut={onSignOut} /> });

  const indexRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/',
    /**
     * INV-APP-084 — the index loader primes **only** the areas list. The
     * area → tracks fan-out stays lazy inside the page (`useQueries` in
     * `useWorkspace`); awaiting it here would let one slow area block the
     * whole calendar behind the route commit.
     */
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

  /*
   * Every settings route renders nothing, deliberately.
   *
   * The URL is the state — which section is open, what a deep link means,
   * what Back does — and `app/shell`'s
   * `SettingsOverlay` is the view of it. The dialog cannot live here: a route
   * component is remounted on every navigation, so moving between the
   * overlay's own sections rebuilt the panel and replayed its entrance
   * animation, which the reader sees as a flash on every click.
   */
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings',
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
    networkRoute, pluginsRoute, appearanceRoute, aboutRoute,
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
    <ConversationProvider>
      <AppShell
        transport={transport}
        unauthorized={unauthorized}
        onOpenSettings={() => go({ name: 'settings' })}
        onOpenPlugins={() => go({ name: 'settings-plugins' })}
        onSignOut={onSignOut}
      />
    </ConversationProvider>
  );
}

/**
 * The conversation module, shared by Today and Track: a list, a `+` in the
 * module head, and the drawer both of them open.
 *
 * A draft is a third open state, not a flag on the second. On a `'rows'` route
 * the `+` cannot create anything: the card is minted by the *first message*, so
 * until one is sent there is no conversation, no card id and nothing to fetch.
 * The drawer is still where it belongs — it is *where a conversation is*, and a
 * conversation being written is one — but it is not a `Conversation`, and
 * modelling it as one would put a null check in every branch that reads one.
 */
function useConversationPanel(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
  source: ConversationPanelSource,
  options?: { showTrack?: boolean },
) {
  /* The drawer is route-local UI. The unfinished work it can show is not: a
     failed draft lives in `ConversationProvider`, keyed by Track, so this
     target can disappear on navigation without taking the retry key with it. */
  const [openTarget, setOpenTarget] = useState<OpenTarget | null>(null);
  /*
   * The conversation whose composer this route was asked to put the caret in
   * — a just-created track's planner row (#1211 S2), and nothing else.
   *
   * It has to be held here rather than read off the registry at render time,
   * because the request is cleared in the same commit that opens the row. Read
   * once, at the composer's mount, and dropped when the drawer closes so that
   * re-opening the same row by hand is an ordinary open.
   */
  const [composerFocusFor, setComposerFocusFor] = useState<string | null>(null);

  const openRowId = openTarget?.kind === 'row' ? openTarget.id : null;
  useEffect(() => { if (openRowId === null) setComposerFocusFor(null); }, [openRowId]);
  const scope: PlannerConversationScope | null = openRowId !== null
    ? source.scopeOf(openRowId)
    : null;
  const routeIntent: ConversationRouteIntent = {
    rows: source.rows, rememberOn: source.rememberOn,
  };

  const rows = source.rows;
  const store = useConversationStore(transport, unauthorized, scope, routeIntent);
  const registry = useConversationRegistry();
  const go = useGo();
  const open = store.conversations.find((conversation) => conversation.id === openRowId) ?? null;

  /*
   * The provider keeps independent drafts for other Tracks, but only this
   * route's slot is visible, reopenable or sendable here.
   */
  const sourceScopeId = source.scopeId;
  const draft = registry.draftOf(sourceScopeId);
  const adoptedDraftId = registry.adoptedDraftIdOf(sourceScopeId);
  const creating = draft?.creating ?? false;
  const discardUnsentDraft = registry.discardUnsentDraft;

  /* Route-local drafts used to disappear automatically on unmount. Preserve
     only work whose request actually left the browser; an untouched or locally
     refused draft has no server identity that needs to outlive this route. */
  useEffect(() => {
    return () => { discardUnsentDraft(sourceScopeId); };
  }, [discardUnsentDraft, sourceScopeId]);

  /*
   * Adoption is the other half of the provider's draft transition. The
   * reducer changes a matching `{ scopeId, key }` from `held` to `adopted` in
   * one step; this route consumes that outcome when its row is available. If
   * the create settles while the route is unmounted, the outcome waits here
   * instead of either opening the wrong Track or being lost.
   */
  useEffect(() => {
    if (adoptedDraftId === null) return;
    if (!rows.some((row) => row.id === adoptedDraftId)) return;
    setOpenTarget({ kind: 'row', id: adoptedDraftId });
    registry.finishDraftAdoption(sourceScopeId, adoptedDraftId);
  }, [adoptedDraftId, registry, rows, sourceScopeId]);

  /*
   * Every write to the draft goes through one of these three, and each is a
   * single whole-object update. `amendDraft` cannot touch the key or the words
   * a POST was made with; the two that can, move both at once.
   *
   * All three are no-ops when the draft they were computed from is no longer
   * the one held in that scope. A write becomes a no-op when the draft was
   * adopted, closed or replaced. `adopt` is guarded by the same identity in
   * the same provider reducer.
   */
  const withDraft = (
    from: ConversationDraftId, next: (current: ConversationDraft) => ConversationDraft,
  ) => {
    registry.editDraft(from, next);
  };
  const amendDraft = (from: ConversationDraft, change: DraftEdit) => {
    withDraft(from, (current) => ({ ...current, ...change }));
  };
  /*
   * The only way to change the key — and it always clears `sentText`.
   *
   * A key is the identity of an attempt and `sentText` is what that attempt
   * sent; carrying one across a change of the other leaves "did the reader edit
   * the words?" comparing a brand-new key against the words some *other* key
   * posted. That mismatch is not a hypothetical: it is what the `'exhausted'`
   * arm used to do.
   */
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

  /*
   * A Track route's planner-open intent asked for a conversation to be opened.
   * The request is consumed against the loaded rows because `scope` does not
   * exist until one of those rows is already open.
   *
   * The condition is "the rows are loaded **and** contain this id", never
   * "the rows do not contain it, so clear". The list arrives a round trip
   * later than the request, and a request cleared while it was still empty is
   * lost for good — the reader lands on the track with the drawer shut and no
   * second chance. The registry is also tab-wide, so the id may belong to
   * another track entirely; that case is not this effect's to decide either, and
   * the route clears it from outside (`TrackRoute`, and the failed-read fallback
   * beside the rows query).
   */
  useEffect(() => {
    const requestedOpenId = registry.requestedOpenId;
    if (requestedOpenId === null) return;
    /* Captured here and not read at render time: the request is cleared in the
       same commit that opens the row, so by the time the composer mounts the
       registry no longer remembers what was asked for. */
    const focusComposer = registry.requestedOpenFocusesComposer;
    if (!rows.some((row) => row.id === requestedOpenId)) return;
    setOpenTarget({ kind: 'row', id: requestedOpenId });
    if (focusComposer) setComposerFocusFor(requestedOpenId);
    registry.clearOpenRequest();
  }, [registry, rows]);

  useEffect(() => {
    if (open === null) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
      if (!store.working || store.stopping) return;
      const target = event.target;
      if (!(target instanceof Element) || target.closest('[role="complementary"]') === null) return;
      /*
       * An open `/` menu owns Escape first, and this listener is the only thing
       * that could take it: it is on `document` in the **capture** phase, so it
       * runs before React ever reaches the composer's own handler and before
       * the drawer's bubble-phase listener. The menu says it is open through
       * the ARIA the composer input already publishes — `useTriggerMenu` puts
       * `role="combobox"` + `aria-expanded` on the editable exactly while the
       * popover is up — so no new marker is minted for this.
       *
       * Order, once the menu is out of the way: Astryx `preventDefault()`s the
       * Escape that closes the menu, and `ui/drawer` skips any
       * `defaultPrevented` Escape, so one press closes the menu and nothing
       * else. A second press then reaches whichever of these two is next.
       */
      if (target.closest('[role="combobox"][aria-expanded="true"]') !== null) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      store.interrupt();
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, [open, store]);

  /*
   * The `+` opens a conversation draft scoped to one concrete Track. Today also
   * satisfies that contract: its route wrapper materialises the launchpad on an
   * explicit press before it calls this function, so this hook never invents or
   * accepts an empty scope id.
   */
  const start = () => {
    /*
     * A draft that was sent and failed is still open business, and `+` is the
     * only way back to it once the drawer was closed. Reopening it — same key,
     * same words, same sentence explaining what went wrong — is what makes the
     * key kept by `closeDrawer` mean anything: without this the next attempt
     * would be a fresh key, and a fresh key on top of an attempt that may have
     * committed is the second conversation this whole mechanism exists to stop.
     */
    if (draft !== null && draft.sentText !== null) {
      setOpenTarget({ kind: 'draft' });
      return;
    }
    /*
     * Otherwise this is a new draft, and the key is minted here, once, for it —
     * not when send is pressed.
     *
     * A key minted per send is a different key on the retry, and a different
     * key is a different derived card: one timeout followed by one retry would
     * leave two conversations holding the same message. Binding it to the
     * draft is the whole reason the server requires the header.
     *
     * A draft held for another scope has another provider slot, so this branch
     * can mint for the current Track without replacing it.
     */
    registry.startDraft({
      scopeId: source.scopeId,
      key: mintIdempotencyKey(),
      text: null, sentText: null, creating: false, error: null, remedy: null,
    });
    setOpenTarget({ kind: 'draft' });
  };

  /*
   * `/new` in the composer runs `start` — the very callback the `+` runs — and
   * every current panel source is a server-backed Track row list, so both entry
   * points create or reopen the same scoped draft.
   *
   * Every current panel source is a concrete Track row list. Today waits until
   * its explicit ensure action has returned the launchpad id before invoking
   * either entry point, so `start` always creates a genuinely scoped draft.
   */
  const startAnother = start;

  /*
   * The attempt `from` became row `row`: forget the draft and open the row.
   *
   * `from` is not decoration. This runs after an `await`, and by then the
   * reader may hold another draft. The provider reducer records the row only
   * if `from` is still held; this route opens only that recorded adoption.
   */
  const adopt = (from: ConversationDraftId, row: Conversation, firstMessage: string | null) => {
    registry.adoptDraft(from, row.id);
    /* The echo is minted from the *answer*, never from the press. That is what
       makes the failure path need no undo: a create that was refused produced
       no row, so there is nothing to record and nothing to roll back — the
       words stay in the draft, where the composer already shows them back. */
    if (firstMessage !== null && firstMessage !== '') {
      registry.noteCreateEcho(row.id, firstMessage);
    }
  };

  const UNCONFIRMED = 'Could not check whether the last attempt went through. Try again in a moment.';

  /*
   * Re-read the list and adopt **this draft's own row** if it is there.
   *
   * A 500, a 503 or a dropped connection does not mean nothing happened: the
   * card can exist with the message already queued behind it. What is not
   * allowed is answering that question with "the list grew". During the seconds
   * an attempt is failing, another tab or another reader can add a conversation
   * to the same Track, and adopting *that* row opens somebody else's chat as if
   * it were the words just typed — while this draft's real card, if it exists,
   * goes unclaimed.
   *
   * So the question asked is the exact one: `trackConversationCardId` is a pure
   * public function of `(scopeId, key)`, golden-tested against the server. The
   * route supplies that derivation (`source.derivedCardId`); this asks it. The
   * row this attempt would have created can be named before looking, and only
   * that id counts.
   *
   * Three answers, not two. `'unknown'` is the re-read *itself* failing, which
   * is the likeliest thing to happen while the network is the reason we are
   * here at all — and it is emphatically not `'absent'`. A caller that treats
   * "I could not look" as "there is nothing there" mints a new key over an
   * attempt that may well have committed, which is the second conversation.
   */
  const adoptIfItLanded = async (
    refresh: () => Promise<readonly Conversation[]>,
    derivedCardId: (idempotencyKey: string) => string,
    scopeId: string,
    key: string,
    /* The words this key posted, or null if it never got that far. A landed row
       means that POST committed, so its message is as delivered as the direct
       success path's — and just as invisible until codex echoes it. */
    sentText: string | null,
  ): Promise<'landed' | 'absent' | 'unknown'> => {
    const rows = await refresh().catch(() => null);
    if (rows === null) return 'unknown';
    const cardId = derivedCardId(key);
    const landed = rows.find((row) => row.id === cardId);
    if (landed === undefined) return 'absent';
    adopt({ scopeId, key }, landed, sentText);
    return 'landed';
  };

  const sendDraft = (text: string) => {
    if (creating || draft === null) return;
    const { create, refresh, scopeId, derivedCardId } = source;
    /*
     * Two different questions, and they are asked of two different strings —
     * because the server asks them that way.
     *
     * `create_track_conversation` refuses `text.trim().is_empty()` and then
     * counts `text.chars().count()` on the **untrimmed** text. So the blank
     * check trims and the length check does not; a message padded to the limit
     * with spaces is over the limit there, and letting it through here would
     * spend a key on a guaranteed 400.
     *
     * And the count is of Unicode scalar values, not UTF-16 code units:
     * `chars()` gives 1 for an emoji where `String.length` gives 2.
     * `Array.from` iterates code points, so it agrees. `.length` did not, and
     * refused legal astral-plane messages at half the real limit.
     */
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
    const previousText = draft.sentText;
    /* The draft this send is *for*, fixed here. Everything below writes through
       it, so a send that outlives its draft — adopted, closed, or left behind by
       a scope switch — changes nothing rather than writing into whatever that
       scope holds by then. */
    let attempt = draft;
    amendDraft(attempt, { text, creating: true, error: null, remedy: null });
    void (async () => {
      try {
        /*
         * Editing the text after a failure is the one case that has to change
         * the key, and it has to look at the list first: the old key may have
         * succeeded with the *old* text and lost its answer, and minting a new
         * key on top of that is how one message becomes two conversations.
         * Only a re-read that came back and said "no new row" earns a new key.
         */
        if (previousText !== null && previousText !== text) {
          const landing = await adoptIfItLanded(
            refresh, derivedCardId, scopeId, attempt.key, previousText,
          );
          if (landing === 'landed') return;
          if (landing === 'unknown') {
            amendDraft(attempt, { error: UNCONFIRMED, remedy: 'retry' });
            return;
          }
          attempt = rekeyDraft(attempt, mintIdempotencyKey());
        }
        markDraftSent(attempt, text);
        attempt = { ...attempt, text, sentText: text };
        adopt(attempt, await create(text, attempt.key), text);
      } catch (error: unknown) {
        attempt = await handleCreateFailure(error, refresh, derivedCardId, scopeId, attempt);
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
  ): Promise<ConversationDraft> {
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
        /* A spent key can never succeed again, so a new one is minted — and it
           takes `sentText` with it: nothing has been posted under this key, so
           the next press must not be read as "the reader changed the words".
           The words themselves are untouched, so that press is a genuinely new
           conversation carrying them. */
        return rekeyDraft(attempt, mintIdempotencyKey(), { error: message, remedy: 'retry' });
      case 'stale-payload':
        amendDraft(attempt, { error: message, remedy: 'new-conversation' });
        return attempt;
      case 'blocked':
        /* Nothing committed and the key is unspent, so both it and the words
           are kept. Whether resending them unchanged can work depends on the
           cause the sentence names — a 400 refuses the words themselves — and
           the composer is open either way. */
        amendDraft(attempt, { error: message, remedy: 'retry' });
        return attempt;
      case 'exists': {
        /* The derived card exists, so this key can never mint again. If the
           re-read turns it up we open it; if it says there is none, only a new
           key can go anywhere and the reader decides whether to spend one. If
           the re-read could not answer, we are not entitled to offer that
           choice yet — a new key here would be a second card next to the one
           the server just told us exists. */
        amendDraft(attempt, { error: message });
        const landing = await adoptIfItLanded(
          refresh, derivedCardId, scopeId, attempt.key, attempt.sentText,
        );
        if (landing === 'absent') amendDraft(attempt, { remedy: 'new-conversation' });
        if (landing === 'unknown') amendDraft(attempt, { error: UNCONFIRMED, remedy: 'retry' });
        return attempt;
      }
      case 'unavailable':
      case 'retry':
        /*
         * Both are ambiguous and both are resolved by looking for *this key's*
         * card. `'unavailable'` used to skip the look on the grounds that a 503
         * means the request was never served — which is not what a 503 means,
         * and on this endpoint it is usually false: the card is minted by the
         * operation runtime and the 503 is raised afterwards, while the first
         * message is being delivered. Skipping the look left that card
         * unadopted; the reason the look was skipped (it might adopt a
         * stranger's row) no longer exists now that the row is named by id.
         *
         * `'absent'` and `'unknown'` end the same way, and safely: the remedy
         * is the same key and the same words again.
         */
        amendDraft(attempt, { error: message });
        if (await adoptIfItLanded(
          refresh, derivedCardId, scopeId, attempt.key, attempt.sentText,
        ) !== 'landed') {
          amendDraft(attempt, { remedy: 'retry' });
        }
        return attempt;
    }
  }

  const sendAsNewConversation = () => {
    if (creating || draft === null || draft.text === null) return;
    const { create, refresh, scopeId, derivedCardId } = source;
    const text = draft.text;
    let attempt = draft;
    amendDraft(attempt, { creating: true, error: null, remedy: null });
    void (async () => {
      try {
        /* Pressed deliberately, but the same fence applies: a new key is only
           safe once the list has actually said the old one produced nothing. */
        const landing = await adoptIfItLanded(
          refresh, derivedCardId, scopeId, attempt.key, attempt.sentText,
        );
        if (landing === 'landed') return;
        if (landing === 'unknown') {
          amendDraft(attempt, { error: UNCONFIRMED, remedy: 'new-conversation' });
          return;
        }
        attempt = rekeyDraft(attempt, mintIdempotencyKey());
        markDraftSent(attempt, text);
        attempt = { ...attempt, text, sentText: text };
        adopt(attempt, await create(text, attempt.key), text);
      } catch (error: unknown) {
        attempt = await handleCreateFailure(error, refresh, derivedCardId, scopeId, attempt);
      } finally {
        amendDraft(attempt, { creating: false });
      }
    })();
  };

  /* Retry means "the same draft again": same key, same words. It is the only
     send path that does not go through the composer, which cleared its field
     the moment the first attempt started. */
  const retryDraft = () => {
    if (draft === null || draft.text === null) return;
    sendDraft(draft.text);
  };

  /* A draft no request was ever made for has no identity worth keeping, and
     that includes one refused locally for being too long. One that was sent
     and failed keeps its key *and* its words: dropping them would make the
     next attempt a second conversation instead of a retry of this one, and
     `start` reopens exactly this state when `+` is pressed again. */
  const closeDrawer = () => {
    setOpenTarget(null);
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
        activeId={open?.id ?? null}
        showTrack={options?.showTrack ?? true}
        onOpen={(conversation) => {
          setOpenTarget({ kind: 'row', id: conversation.id });
        }}
      />
    ),
    /* The module head's action, composed by the page — same slot the TRACKS and
       CARDS modules already use, which is why this needed no new mechanism.
     *
     * `plus`, not `chat`. This drew the speech bubble until owner tried to add a
     * conversation on Today and did not recognise the control as an add — the
     * label said `New conversation` and it worked, but every other "make a new
     * one" in the app is a `+`: `New area` and `New track in {area}` in the
     * shell sidebar. The bubble named the *noun*
     * while the rest of the app names the *verb*, so it read as a decoration of
     * the module title rather than as the module's action.
     *
     * One element, and since #1341 both `'rows'` routes render it — Today and the
     * track page — so this is the same symbol in both places rather than two
     * that agree by coincidence.
     */
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
            {/* The strip is welded to the well's top edge, so it renders
                *before* the composer. Each child keeps the condition it had:
                a remedy can be offered with no error beside it. */}
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
            {/* Offered on a draft too, and it means the same thing the `+`
                means there: throw this unsent draft away and begin another.
                Same callback, so the two cannot disagree about that. */}
            <ChatComposer disabled={creating} onSend={sendDraft} onNewConversation={startAnother} />
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
            {store.actionError !== null && (
              <ChatFooterNotice><ChatFooterError message={store.actionError} /></ChatFooterNotice>
            )}
            <ChatComposer
              /* Read at mount only, which is what makes it one-shot: the
                 composer mounts when the drawer opens on a row, and the flag
                 is dropped when it closes (the effect beside
                 `composerFocusFor`). #1211 S2 — a track created from the `+`
                 lands with its planner conversation open and the caret in it:
                 that thread is where the intent was delivered (#1299) and
                 where the next thing the reader says goes. */
              focusOnMount={composerFocusFor === open.id}
              disabled={store.sendBlocked || !store.historyReady}
              onSend={(text) => store.send(open.id, text)}
              /* `stopping` keeps Stop *shown* while the interrupt is in flight;
                 it is not passed down as a prop of its own, because the composer
                 cannot make Astryx's Stop unavailable and `interrupt()` above
                 already refuses a second one. */
              onStop={store.working || store.stopping ? store.interrupt : undefined}
              onNewConversation={startAnother}
            />
          </>
        )}
      >
        {/* The words are shown back because the composer clears its field on
            send and a failed draft would otherwise be gone. Nothing here claims
            they arrived — only `Sending…` while the request is open, and the
            alert below when it came back. */}
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
            {/*
              * Keyed on the conversation, so switching threads in place builds
              * a new transcript rather than reusing the old one's state.
              *
              * The drawer stays mounted across a switch — same route, same
              * `<Drawer>` — so without the key `ChatThread` is reused, and the
              * refs its follow-the-newest-turn effect carries are reused with
              * it. The effect re-runs (its deps are `[turns.length, newestId]`
              * and the newest id changed), and then asks `followsNewest`, whose
              * answer is about A: a reader parked in the middle of A opens B
              * parked too. `area-conversation.test.tsx` holds that down with
              * two primed two-turn transcripts, so the switch cannot remount
              * the component for some other reason and pass anyway.
              *
              * The rail's own state goes with the instance too — the lit
              * exchange, the roving tab stop, the installed listeners — and
              * that is state about A being applied to B's dots. It cannot
              * survive as A's *words*: every label is derived from B's current
              * exchanges. What it produces is a rail with nothing lit, or, if
              * the two conversations happen to share a turn id, one lit for the
              * wrong reason. The same treatment the sibling state above already
              * gets on `[cardId]`; this was the one place it was left out.
              */}
            {/*
              * Not while the first page is unknown and there is nothing to
              * show (#1449).
              *
              * `ChatThread` renders its empty state for an empty turn list,
              * and it draws the live `Working` dot in that state. Mounted
              * unconditionally, it painted that state *under* `Loading conversation…`
              * above — the surface saying "I have not read this thread" and
              * "this thread is empty" in the same frame, one of which is not a
              * claim it is entitled to make.
              *
              * The condition is the invariant and not a proxy for it: the
              * empty state is reachable only once the read has answered.
              * `turnsOf` is the second arm rather than a redundancy — a reopen
              * whose query was collected renders the remembered transcript
              * while a fresh read is in flight (`serverEntries`'s fallback),
              * and that thread has words in it, so gating it away would blank
              * a conversation the tab can already show.
              */}
            {(store.historyReady || store.turnsOf(open.id).length > 0) && (
              <ChatThread
                key={open.id}
                conversation={open}
                turns={store.turnsOf(open.id)}
                pending={store.pending.has(open.id)}
              />
            )}
            {/*
              * Nothing follows the transcript.
              *
              * `Reset conversation` used to be here, one line under the last
              * reply. It is gone from the product (#1139), not moved: an area's
              * chat track holds as many conversations as you start, so "empty
              * this one in place" was never the answer to a thread going
              * wrong — opening another one is, and the old thread stays
              * readable in the list. The endpoint behind it is still served;
              * nothing in the browser calls it. The new-conversation door that
              * replaces it is `/new` in the composer below, which is reachable
              * from *inside* the drawer, where the `+` is not.
              */}
          </>
        )}
      </Drawer>
    ),
  };
}

function TodayRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const workspace = useWorkspace(transport, unauthorized);
  const go = useGo();
  const trackMutations = useTrackMutations(transport, unauthorized);
  const deletion = useDeleteConfirm((trackId, signal) => {
    const track = workspace.tracks.find((candidate) => candidate.id === trackId);
    if (track === undefined) throw new Error('This track is no longer available.');
    return trackMutations.remove(track.id, track.areaId, signal);
  });
  /*
   * #1253 §5.1 — the launchpad resolve, and it is a READ.
   *
   * `POST /api/today/launchpad/ensure` is deliberately not called from here,
   * and that is INV-TODAYDOC-001, not a nicety: `ensure` materializes a
   * workspace and then submits a `planner-harness-start` operation and waits on
   * it, so putting it on the page-load path would make Today fail hard
   * whenever codex is down — worse than the Today this replaces, which needed
   * nothing to render. `ensure` belongs to an explicit action; the
   * Conversations `+` below is that action when no launchpad exists yet.
   *
   * "Nothing yet" arrives as `null` in the body and becomes the empty state.
   * Every failure — including a 404, which no longer means anything special
   * here — arrives as an error and is rendered as one (INV-TODAYDOC-002).
   */
  const launchpadQuery = useQuery(todayLaunchpadQueryOptions(transport, unauthorized));
  const launchpad = launchpadQuery.data;
  const launchpadTrackId = launchpad?.track_id ?? '';
  const [preparedLaunchpadTrackId, setPreparedLaunchpadTrackId] = useState<string | null>(null);
  const [conversationStartRequested, setConversationStartRequested] = useState(false);
  const conversationTrackId = launchpadTrackId || preparedLaunchpadTrackId || '';
  const launchpadEnsure = useTodayLaunchpadEnsureMutation(transport, unauthorized);
  /*
   * #1343 — Reset. `POST /api/today/launchpad/report/reset`, no body.
   *
   * It replaces the deleted `Rewrite today's progress` trigger, and it is the
   * opposite act: that one asked an agent to fill the document, this one
   * empties it so the owner can watch the flow run again from the empty state.
   * Today's activity now reaches an agent by a different route entirely —
   * starting a conversation on the launchpad, where the server injects the
   * day's window — so nothing on this page asks for a write any more.
   *
   * It is destructive and irreversible from the UI, so it goes through
   * `useDeleteConfirm` + `ConfirmDialog`, the same shape the track delete on
   * this very route uses. The hook is keyed by an id; the launchpad track id is
   * what it gets, which is also what makes the control unavailable before the
   * resolve has answered.
   */
  const reportReset = useTodayReportResetMutation(transport, unauthorized);
  const resetConfirm = useDeleteConfirm(() => reportReset.reset());
  /*
   * ── The Conversations module (#1341) ─────────────────────────────────────
   *
   * **The launchpad track's own conversations**, read from the server by the
   * same rule the track route uses, and it is worth saying what it replaced
   * because the two rules have nothing in common.
   *
   * It used to be `'elsewhere'` with `intent: 'all'`, which reads the session
   * registry: every conversation *this browser tab* had opened, on any track,
   * each row carrying a `, on <track>` suffix. That is a cross-track visiting
   * history, not a list of anything, and it made Today the one surface whose
   * Conversations module answered a different question from every other
   * surface's. Owner's call (#1341): Today and a track page say the same
   * sentence — "the conversations of the track you are looking at" — and Today
   * is looking at the launchpad, whose report is the document above.
   *
   * The concrete thing that was broken by the old rule, and is fixed by this
   * one: `POST /api/today/summary` creates exactly one conversation on the
   * launchpad and that conversation *is* what the reader asked for — it is what
   * writes the report. Nothing in the tab had ever opened it, so the registry
   * had never heard of it, so it appeared nowhere: the endpoint's own module
   * doc promised it was "openable in Today's Conversations module" and the
   * frontend did not deliver that. Verified failing before this change, in
   * `today-conversation.test.tsx`.
   *
   * A cross-track index is not lost, it is *moved*: it becomes its own card
   * holding everything about one track, on its own issue. It is deliberately
   * not squeezed back in here.
   */
  const launchpadConversationsQuery = useQuery({
    ...trackConversationsQueryOptions(transport, conversationTrackId, unauthorized),
    /* No launchpad, no list — and above all no request. A fresh workspace
       resolves to `null`, and an ungated read would ask the server about a
       track named `''` on every first-run page load. */
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
  const chat = useConversationPanel(
    transport,
    unauthorized,
    {
      scopeId: conversationTrackId,
      rows: launchpadRows,
      /*
       * The launchpad is a real track and these rows are its own, so this route
       * says so — the same statement `TrackRouteBody` makes about itself, and
       * the store checks every row against it rather than trusting the claim.
       *
       * It is not decoration here: it is what lets the *open* conversation be
       * remembered, and that entry has live readers — the transcript the drawer
       * falls back to while a reopen is settling, and `turnsBefore` in `send`,
       * which is how a message the reader really did send twice is counted
       * twice. A row whose own `trackId` does not match this scope is rejected
       * by that same check.
       */
      rememberOn: conversationTrackId,
      derivedCardId: (idempotencyKey) => trackConversationCardId(conversationTrackId, idempotencyKey),
      scopeOf: (conversationId) => {
        const row = launchpadRows.find((candidate) => candidate.id === conversationId);
        /* `id: row.trackId`, never `launchpadTrackId` — see the same line on the
           track route. Written the other way the `rememberOn` comparison
           compares a value with itself and stops being a comparison. */
        return row === undefined ? null : {
          id: row.trackId, title: row.trackTitle, cardId: row.id, cardTitle: row.title,
          updatedAt: row.updatedAt, kind: row.kind, state: row.state,
        };
      },
      create: launchpadConversationMutations.create,
      refresh: launchpadConversationMutations.refresh,
    },
    /* Every row on this list is on the launchpad, and the launchpad is what
       this page is. Naming it on each row spends the column saying one thing N
       times — the same reason the track route hides it. */
    { showTrack: false },
  );

  const startTodayConversation = () => {
    if (launchpadEnsure.pending) return;
    if (conversationTrackId !== '') {
      /* `ensure` can materialise the launchpad and still return an error when
         its harness start fails. The resolve then discovers the real track.
         A retry in that state opens the now-available draft and dismisses the
         stale mutation error; it must not ask `ensure` to create it again. */
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
    /* The outer resolve is still unknown or failed; neither answer means an
       empty conversation list. Its own error is already rendered in the
       document region. */
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
  /* The document itself comes from the ordinary track detail — the resolve
     carries no `report_card_id` because `readTrackReport` locates the card by
     `kind === 'track-report'` and that field would have no consumer (§5.1).

     Gated on the server's own answer, not merely on having a track id: when
     `report_has_noninitial_content` is false there is nothing to draw, so the
     page load stays at one request. It also keeps the states below honest —
     every one of them is then about a document the reader is actually owed. */
  const launchpadHasContent = launchpad?.report_has_noninitial_content === true;
  const launchpadDetailQuery = useQuery({
    ...trackDetailQueryOptions(transport, launchpadTrackId, unauthorized),
    enabled: launchpadTrackId !== '' && launchpadHasContent,
  });
  const launchpadReport = useMemo(
    () => readTrackReport(launchpadDetailQuery.data?.cards ?? []),
    [launchpadDetailQuery.data],
  );
  /*
   * Three states, three answers — and they must not be collapsed.
   *
   * `readTrackReport(...) === null` is true in all three: while the detail is
   * in flight (which is EVERY page load, because this query cannot start until
   * the resolve has answered), when the detail read fails, and when the
   * payload genuinely will not decode. Handing all three to `ReportDocument`'s
   * `empty` told a reader whose server was unreachable that their build was
   * too old, with no retry — the same silent-degradation INV-TODAYDOC-002
   * forbids, just with a worse lie in place of the empty state.
   */
  const launchpadDocument = launchpadDetailQuery.isError
    ? (
      <ErrorBox
        message={`Today's progress is unavailable: ${launchpadDetailQuery.error.message}`}
        onRetry={() => { void launchpadDetailQuery.refetch(); }}
      />
    )
    : launchpadDetailQuery.data === undefined
      // In flight. Nothing, not a placeholder: this frame is one round trip
      // long on a healthy server, and a skeleton that flashes on every load is
      // more motion than information.
      ? null
      : (
        <ReportDocument
          report={launchpadReport}
          /* The detail has arrived and the server says the report has content,
             so the in-flight and read-failed states are both behind us. What
             remains is "this build could not make a report out of what
             arrived" — almost always an undecodable payload, but also a 200
             carrying no `kind === 'track-report'` card at all. That second
             shape is effectively unreachable (the card is `deletable: false`)
             and its wording would be slightly off if it happened; it is not
             worth a third branch, but it is worth not claiming a universal the
             code does not enforce. */
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
    {/* A failed reset is announced where a failed delete is, and the document
        behind it is unchanged: the server wrote nothing. */}
    {resetConfirm.feedback.error !== null && <div role="alert" data-nc-error-box="">
      <span>{resetConfirm.feedback.error}</span>
      <button type="button" data-nc-action="tertiary" onClick={resetConfirm.feedback.clear}>Dismiss</button>
    </div>}
    <TodayPage
      tracks={workspace.tracks}
      areas={workspace.areas}
      // The row belongs to features/track and Today may not import a sibling
      // domain, so the composition layer injects it. One TrackRow still, per
      // INV-DUP-009.
      renderTrackRow={(track, options) => (
        <TrackRow
          track={track}
          variant={options.variant}
          hourLabel={options.hourLabel}
          areaName={options.areaName}
          onOpen={(trackId) => go({ name: 'track', trackId })}
          /* The panel variant only — that is the calendar's agenda, inside the
             card, where every other list already puts a delete under the status
             dot. The main column's sections stay read-only: they are the day's
             report, and a report is not a place you edit from. */
          onDelete={options.variant === 'panel' ? deletion.request : undefined}
        />
      )}
      conversationList={conversationList}
      /*
       * The `+`, which Today did not have and now does (#1341).
       *
       * A Today conversation attaches to the launchpad, the track whose report
       * is the document above. Once that track exists this is the ordinary
       * conversation action; starting one here means "ask about my day", and it
       * lands exactly where the day already lives.
       *
       * With no launchpad yet the same slot remains visible and says what it
       * starts. Its press explicitly calls `POST /api/today/launchpad/ensure`,
       * then opens the draft on the returned track. The page load remains a
       * pure read; the workspace and harness are attributable to that press.
       */
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
      /* Undefined while the resolve is in flight, `null` when the server says
         there is no launchpad yet. The page
         decides the empty state from `report_has_noninitial_content` and from
         nothing else — see INV-TODAYDOC-003 on `TodayPageProps.launchpad`. */
      launchpad={launchpadQuery.isError ? undefined : launchpad}
      launchpadDocument={launchpadDocument}
      launchpadError={launchpadQuery.isError
        ? <ErrorBox
          message={`Today's progress is unavailable: ${launchpadQuery.error.message}`}
          onRetry={() => { void launchpadQuery.refetch(); }}
        />
        : undefined}
      /* Rendered only beside a written document — `TodayPage` decides that,
         because it is the same `report_has_noninitial_content` branch the
         empty state is on and duplicating the condition here would be two
         readings of one predicate. */
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
 * `/area/$areaId/new` — the page you start a track on (#1211).
 *
 * It owns the whole create, which used to be split between the shell (the POST,
 * the 409, the navigation) and a dialog inside it. There is no reason for the
 * shell to hold any of it now that the surface is a route: an Area group's `+`
 * navigates here, and one route owning one operation is the shape every other
 * write in this file already has.
 *
 * ## How the first message is delivered: on the create itself (#1299)
 *
 * The composer's sentence is the track's *intent*, and its destination is the
 * track's planner agent as the first message. It travels as `first_message` on
 * this one POST, and the reason it travels there and nowhere else is worth
 * stating so nobody moves it back into this component.
 *
 * Doing it from this page took three writes — create, read the detail to find
 * the planner card, post the message — and two review rounds established that
 * the sequence cannot be made sound from a component:
 *
 *  * the reader can navigate away mid-flight; the requests are not cancelled,
 *    the route unmounts, and the track exists with the sentence lost and nothing
 *    said; and
 *  * `POST /api/cards/{id}/planner/input` carries no idempotency key, and the
 *    server enqueues *before* it writes audit and responds — so a lost response
 *    or a 500-after-enqueue makes any retry deliver the same sentence twice.
 *
 * Neither was a defect in this file; both are what running a distributed
 * transaction in a component costs. So the kernel took the write: `POST
 * /api/tracks` validates the sentence before anything is minted and seeds it as
 * an `Observation::UserMessage` inside the same `planner-harness-start`
 * transaction that installs the harness — one write, delivered exactly once,
 * attributed to the human. Both failure classes stopped existing rather than
 * being defended against.
 *
 * What this route still owes the reader is the landing: it puts them on the
 * track with the planner conversation **already open and holding the caret**,
 * which is now where the agent's answer arrives and where the next thing they
 * say goes. That landing is stated on the navigation itself (`openPlanner`),
 * and its only effect is a drawer.
 *
 * The create is safely retryable under the draft-scoped key below. Ambiguous
 * failures preserve that key for an explicit retry, an exhausted key is replaced
 * before the next submit, and a payload conflict offers a separate explicit
 * "Start as a new track" choice. Nothing silently changes identity or submits on
 * the reader's behalf.
 *
 * The create posts **no title** — the kernel stores the empty string and the
 * planner agent names the track through `calm.track.rename` once it knows what the
 * work is (#1211 S1).
 */
function NewTrackRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const areaId = useRouteParam('/area/');
  const workspace = useWorkspace(transport, unauthorized);
  const trackMutations = useTrackMutations(transport, unauthorized);
  const templates = useTrackTemplates(transport, unauthorized);
  const recipes = useTrackRecipes(transport, unauthorized);
  const go = useGo();
  const [creating, setCreating] = useState(false);
  /*
   * #1384 — the `Idempotency-Key` for this page's create, minted **once for the
   * draft** rather than per submit.
   *
   * A key minted per submit is a different key on the retry, and a different
   * key mints a second track holding the same sentence — which is the failure
   * the header exists to stop, reintroduced at the caller. Same rule the
   * conversation drawer already follows for its own draft.
   *
   * Mount-scoped is the normal draft lifetime: success navigates away, while
   * an ambiguous failure preserves the key for a safe explicit retry. The one
   * state that replaces it in-place is structured `idempotency_key_exhausted`:
   * the server has proved that key can never recover, so the next user submit
   * gets a fresh one (#1435).
   *
   * Minted off `getRandomValues` (via `mintIdempotencyKey`) because the app is
   * served over plain http on the LAN, where `crypto.randomUUID` does not
   * exist.
   */
  const [createKey, setCreateKey] = useState(mintIdempotencyKey);
  const [error, setError] = useState<string | null>(null);
  const [canRetryAsNewTrack, setCanRetryAsNewTrack] = useState(false);
  const [folderConflictRecovery, setFolderConflictRecovery] = useState<Readonly<{
    areaId: string;
    areaName: string;
    cwd: string;
  }> | null>(null);
  const listDirectory = createDirectoryLister(transport, unauthorized);
  const area = areaId === undefined
    ? undefined
    : workspace.areas.find((candidate) => candidate.id === areaId);
  /*
   * "Is this route still the screen?" — read by the create continuation, which
   * outlives the route whenever the reader navigates during a slow POST.
   *
   * The mount arm setting it back to `true` is load-bearing, not symmetry.
   * React's StrictMode double-invokes effects in development (mount → cleanup →
   * mount), so with only a cleanup arm the flag latched `false` on the very
   * first render and *every* create silently stopped navigating. jsdom does not
   * run StrictMode here, so the unit suite stayed green — the real-kernel e2e
   * is what caught it.
   */
  const liveRef = useRef(true);
  useEffect(() => {
    liveRef.current = true;
    return () => { liveRef.current = false; };
  }, []);

  /*
   * Landing in the planner conversation is stated on the **navigation**, not read
   * here (#1211 S2, `usePlannerOpenIntent`).
   *
   * This route cannot name the card to open — `POST /api/tracks` answers with a
   * `Track`, and the planner card arrives a route later with the track detail — so
   * an earlier shape of this slice read the detail here, raced it against a
   * deadline, and wrote the card id into the conversation registry before
   * navigating. That registry outlives every route, which is what made the
   * write unsound in a way a deadline cannot fix: a landing that never reaches
   * the track (a failing detail read, an error box) leaves the request standing,
   * and it springs a drawer open on some later visit nobody asked for.
   *
   * `openPlanner` puts the intent on the history entry this navigation creates, so
   * it is scoped to exactly one landing, is redeemed by the track route body
   * against its own cards, and cannot be seen — or cleared — by any other
   * route. `focusComposer` comes with it: the sentence has already been
   * delivered into that conversation, so the caret belongs where its answer
   * lands and where the next thing the reader says goes.
   */
  const submit = (draft: NewTrackDraft, targetAreaId = areaId, attemptKey = createKey) => {
    if (targetAreaId === undefined) return;
    setCreating(true);
    setError(null);
    setCanRetryAsNewTrack(false);
    setFolderConflictRecovery(null);
    const messageIsBlank = isBlankForKernel(draft.message);
    const body = {
      area_id: targetAreaId,
      /* No `title` (#1211): the sentence the reader typed is the track's intent,
         not its name. It rides on `first_message` below, and the landing still
         opens the planner composer — now for the *reply*, not for a retype. */
      theme: readHostThemeRgb(),
      /*
       * #1299 — the sentence, on the create that makes the track.
       *
       * Two separate decisions, and only the first one touches whitespace.
       *
       * *Whether* the key rides at all is decided by the two typed calls below:
       * "the reader said nothing" is the **absent field and key**, not `''`.
       * The kernel validates this field before it mints
       * anything and 400s a blank one, so posting an empty string would turn
       * "opened the page and pressed nothing" into a failed create. Blank is
       * `isBlankForKernel` — the kernel's own criterion, written once in
       * `core/domain/track.ts` and asked here and in `NewTrackForm` alike, so
       * the enabled Create and the sent request can never disagree about what
       * counts as empty.
       *
       * *What* rides is `draft.message` untouched. The kernel forwards the
       * text to the agent verbatim and hashes it verbatim, so a trim here
       * would deliver a sentence the reader did not type — and it would do it
       * invisibly, since the composer still shows theirs.
       */
      // Spread, not two optional fields: no template leaves both keys absent,
      // and `template_id: undefined` is not the same request as no
      // `template_id` for anything that inspects the object before it is
      // serialized.
      ...(draft.template_id === undefined ? {} : { template_id: draft.template_id }),
      ...(draft.template_input === undefined ? {} : { template_input: draft.template_input }),
      /* #1292 — the third starting point, spread the same way and for the same
         reason. It is never present at the same time as `template_id`: the
         draft comes from a tagged union with one arm at a time, and the kernel
         answers a request naming both with a 400. */
      ...(draft.recipe_id === undefined ? {} : { recipe_id: draft.recipe_id }),
      /*
       * Both keys or neither. `cwd` without `attach_folder` means "this path is
       * already claimed by some area", which the kernel answers with a 409
       * whenever it is not — so the omitted-flag default is a request that
       * fails for every folder the user has not already bound. `true` is what
       * "I picked this folder for this area" means, and it is a no-op when this
       * area already covers the path (`tracks.rs`'s same-area arm), so a second
       * track in the same repository does not conflict with the first.
       */
      ...(draft.cwd === undefined ? {} : { cwd: draft.cwd, attach_folder: true }),
    } satisfies NewTrackBodyWithoutFirstMessage;
    /*
     * #1384 / #1436 — the two calls are distinct overloads. The keyed one
     * cannot be constructed without both `first_message` and its key; the
     * message-less one cannot accidentally advertise idempotency it does not
     * have.
     */
    const keyForAttempt = messageIsBlank ? undefined : attemptKey;
    const creation = messageIsBlank
      ? trackMutations.create(body)
      : trackMutations.create({ ...body, first_message: draft.message }, attemptKey);
    void creation.then((track) => {
      /*
       * And only if the reader is still here.
       *
       * `POST /api/tracks` can be slow, and nothing stops them pressing Back or
       * picking a rail row while it is in flight. This route unmounts, but the
       * promise continuation still runs — and an unguarded `go()` yanked them
       * off the page they had just chosen and onto the track. The track is
       * created either way and is in the rail; what they lose by not being
       * navigated is nothing, and what they lose by being navigated is their
       * own last action.
       *
       * **Known gap, #1299.** `liveRef` answers "did this route unmount", which
       * is not the same question as "is this still the reader's surface". The
       * mobile sheets (Pages / Areas) do not unmount the outlet — they cover it
       * behind an `inert` `main` — so a create landing while a sheet is open
       * still passes this guard and navigates underneath it. An earlier version
       * of this comment claimed the dock was covered; it is not, and the fix is
       * a real "is this surface current" signal rather than a mount flag.
       */
      if (!liveRef.current) return;
      /*
       * The sentence rides along (#1449).
       *
       * The card that holds this message does not exist as far as this route
       * is concerned — `POST /api/tracks` answers with a `Track` — and the
       * kernel's transcript will not carry the message either until codex
       * echoes the turn back (the transcript table is written only there). So the
       * words travel on the entry this navigation creates, and the track route
       * mints the optimistic echo once it knows its planner card. Same
       * one-landing scope as `openPlanner` itself, and struck off by the same
       * `disarm()`.
       */
      go({
        name: 'track',
        trackId: track.id,
        openPlanner: true,
        ...(messageIsBlank ? {} : { openPlannerMessage: draft.message }),
      });
    }).catch((failure: unknown) => {
      const conflict = folderConflictOf(failure);
      if (conflict !== null) {
        // The 409 body names an area by id and carries no `error` key, so the
        // generic message below would be the bare word "Conflict".
        const owner = workspace.areas.find((candidate) => candidate.id === conflict.area_id);
        setError(folderConflictMessage(conflict, owner?.name ?? null));
        if (owner !== undefined && owner.id !== targetAreaId
          && conflict.conflict_kind !== 'ancestor' && draft.cwd !== undefined) {
          setFolderConflictRecovery({ areaId: owner.id, areaName: owner.name, cwd: draft.cwd });
        }
        return;
      }
      if (failure instanceof ApiError && keyForAttempt !== undefined && liveRef.current) {
        const keyAction = trackCreateKeyAction(failure.failure);
        if (keyAction === 'replace') {
          // Replace only the key this response spent. `creating` serializes
          // submits today; the equality check keeps a late response safe if
          // that policy changes later.
          setCreateKey((current) => current === keyForAttempt ? mintIdempotencyKey() : current);
        } else if (keyAction === 'offer-explicit-replace') {
          setCanRetryAsNewTrack(true);
        }
      }
      setError(failure instanceof ApiError ? failure.message : 'Could not create the track.');
    }).finally(() => { setCreating(false); });
  };

  const retryAsNewTrack = () => {
    setCreateKey(mintIdempotencyKey());
    setCanRetryAsNewTrack(false);
    setError(null);
  };

  const recoverFolderConflict = (draft: NewTrackDraft) => {
    if (folderConflictRecovery === null || draft.cwd !== folderConflictRecovery.cwd) return;
    const nextKey = mintIdempotencyKey();
    setCreateKey(nextKey);
    submit(draft, folderConflictRecovery.areaId, nextKey);
  };

  /*
   * A syntactically fine id for an area that has been deleted must not render a
   * working composer: the reader types a sentence, presses Enter and only then
   * eats a 4xx. The rail's own area list answers it, so no extra read.
   *
   * Three states of that read, three answers — and the composer is behind all
   * of them. An earlier cut computed one `areaResolved` flag and refused only
   * the settled-and-absent case, letting the other two **fall through to the
   * form**: a cold deep link whose `GET /api/areas` was still in flight got a
   * submittable composer with no answer behind it, and a 500 on that read
   * (`areas: []`, not loading, error set) got one *permanently*. Both are the
   * 4xx-after-typing this check exists to prevent, so the fall-through is now a
   * refusal in each direction.
   *
   * `areasLoading` is `areasQuery.isLoading`, which TanStack v5 derives as
   * `isPending && isFetching` — a read with **no cached list at all** that is
   * currently fetching. A background refetch over a cached list is
   * `isRefetching`, not `isLoading`, so this branch cannot swallow the composer
   * on every revalidation; it is the first paint only.
   */
  if (areaId === undefined) return <ErrorBox message="This area could not be found." onRetry={() => { go({ name: 'today' }); }} />;
  /* In flight, nothing cached. Nothing on screen rather than a skeleton, the
     same answer the index route gives while this exact list loads: this frame
     is one round trip long on a healthy server. */
  if (workspace.areasLoading) return null;
  /* The read failed, so "is this area still there" has no answer. The list is
     the rail's, so this is the rail's own message and its own retry — not a
     "could not be found" the server never said. */
  if (workspace.areasError !== null) return <ErrorBox message={workspace.areasError.message} onRetry={workspace.retryAreas} />;
  /* Settled and successful: `[]` now means empty, and absence means deleted. */
  if (area === undefined) return <ErrorBox message="This area could not be found." onRetry={() => { go({ name: 'today' }); }} />;
  return (
    <NewTrackForm
      submitting={creating}
      error={error}
      templates={templates.templates}
      templatesLoaded={templates.loaded}
      templatesError={templates.error}
      recipes={recipes.recipes}
      errorAction={folderConflictRecovery === null
        ? canRetryAsNewTrack
          ? { label: 'Start as a new track', onClick: retryAsNewTrack }
          : undefined
        : {
          label: `Create in ${folderConflictRecovery.areaName}`,
          isApplicable: (draft) => draft.cwd === folderConflictRecovery.cwd,
          onClick: recoverFolderConflict,
        }}
      onManageRecipes={() => go({ name: 'recipes' })}
      initialTemplateId={area.defaultTemplateId}
      initialCwd={area.defaultCwd}
      listDirectory={listDirectory}
      onSubmit={submit}
    />
  );
}

/**
 * `/recipes` — the reader's own saved starting points (#1292 S4).
 *
 * This route owns exactly two things the feature module must not: the
 * transport, and the translation of a rejected write into the outcome the
 * editor branches on.
 *
 * That translation is the reason `RecipeWriteOutcome` exists. A 409 is not a
 * failure the editor reports and moves on from — it is the one status whose
 * handling is "stay in edit mode and keep every character the author typed" —
 * and deciding that by matching on an error message inside the feature would
 * make a user-visible behaviour depend on wording. `ApiError.failure.status`
 * is decided here, where the transport's own vocabulary already lives.
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
      /* Everything else is reported verbatim, including the 400 a malformed
         fence earns: the kernel's message names the fence that would not
         parse, and paraphrasing it here would lose the only part the author
         can act on. */
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

/*
 * Split in two on purpose. The conversation panel's `+` needs the track in
 * scope, and the track is only known after the detail query resolves and three
 * early returns have run — a hook cannot live below those. So this half owns
 * the fetching and the returns, and the half below owns the hooks that need a
 * track.
 */
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
  /*
   * The card Today asked for, if this track has it and it is a conversation card
   * at all. **Both** conversation markers, not just the planner one (#1189 §5.2):
   * an assistant card is a `codex` card carrying `harness_profile: 'assistant'`,
   * and while this predicate said `isPlannerHarnessPayload` alone every assistant
   * request answered `undefined` here and was cleared by the effect below —
   * before `TrackRouteBody`'s conversation list had even loaded. The Today →
   * assistant path was cut here, one level above the effect that consumes it,
   * and no change down there could have reached it.
   *
   * Reading the markers off the track detail is also what makes the consuming
   * effect's "wait for the rows" honest: the card and the row are two views of
   * one thing, and the card arrives with the route.
   *
   * Since #1341 this is a **fail-safe with no producer**, and that is stated
   * rather than left to be discovered. Today was the only surface that left a
   * request for a card the arriving track might not have; it lists the
   * launchpad's own conversations now and opens them in place, so it leaves
   * none. The one live producer left is #1211's planner-open intent below, which
   * names a card of this very route and returns early when there is no planner
   * card — it cannot produce the case this clears. Kept because a stale request
   * springing the drawer open on a later visit is the failure it prevents, and
   * because the cross-track conversation card (its own issue) is a producer
   * coming back.
   */
  const requestedCard = detail.data?.cards.find((card) => card.id === registry.requestedOpenId
    && card.kind === 'codex'
    && (isPlannerHarnessPayload(card.payload) || isAssistantHarnessPayload(card.payload)));
  const detailMatchesRoute = trackId !== undefined && detail.data?.track.id === trackId;
  useEffect(() => {
    if (registry.requestedOpenId === null || detail.isLoading || detail.isFetching) return;
    if (!detailMatchesRoute || requestedCard === undefined) registry.clearOpenRequest();
  }, [detail.isFetching, detail.isLoading, detailMatchesRoute, registry, requestedCard]);

  if (!detail.data) {
    if (detail.isLoading || detail.isFetching) return null;
    if (detail.error instanceof Error) return <ErrorBox message={detail.error.message} onRetry={() => { void detail.refetch(); }} />;
    return <PendingRoute label="Track" owner="features/track" missing />;
  }
  // `detail.data` can still be the previously-viewed track while this one
  // fetches; rendering it under this URL would show the wrong track.
  if (trackId !== undefined && detail.data.track.id !== trackId) return null;

  const detailActivity = trackActivityFrom(detail.data.track.id, detail.data.overlays);
  const track = toTrack(detail.data.track, detailActivity);

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

function cardInputNotifications(
  cards: TrackDetailWire['cards'], overlays: TrackDetailWire['overlays'],
): readonly TrackInputNotification[] {
  const statusByCard = new Map<string, TrackInputNotification>();
  for (const overlay of overlays) {
    if (overlay.plugin_id !== 'kernel' || overlay.entity_kind !== 'card' || overlay.kind !== 'status') continue;
    if (typeof overlay.payload !== 'object' || overlay.payload === null) continue;
    const state = (overlay.payload as Record<string, unknown>).state;
    if (state !== 'AwaitingInput' && state !== 'Errored') continue;
    const card = cards.find((candidate) => candidate.id === overlay.entity_id);
    if (card === undefined) continue;
    const source = card.kind === 'codex' && isPlannerHarnessPayload(card.payload)
      ? 'Planner'
      : card.kind === 'codex' && isAssistantHarnessPayload(card.payload)
        ? card.title ?? 'Assistant'
        : card.title ?? card.kind;
    const current = statusByCard.get(card.id);
    if (current !== undefined && current.updatedAt >= overlay.updated_at) continue;
    statusByCard.set(card.id, {
      cardId: card.id,
      source,
      message: state === 'AwaitingInput'
        ? 'Requires input to continue.'
        : 'Stopped with an error and needs attention.',
      state: state === 'AwaitingInput' ? 'awaiting-input' : 'errored',
      updatedAt: overlay.updated_at,
    });
  }
  return [...statusByCard.values()].sort((left, right) => right.updatedAt - left.updatedAt);
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
  const trackMutations = useTrackMutations(transport, unauthorized);
  const conversationMutations = useTrackConversationMutations(transport, track.id, unauthorized);
  const openMobileSection = useOpenMobileSection();
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
  /*
   * The URL is read, validated and turned into props **here**, in `app/**`:
   * `features-no-app` is an error-level dependency-cruiser rule, so `TrackPage`
   * cannot reach the router at all and stays a pure renderer (#1191 §2.4).
   *
   * A live `?card=` wins over `?panel=`. The two describe one surface and
   * `buildTrackSearch` already refuses to emit both, so this only decides what a
   * hand-edited URL means — and it means the card, the older deep-linkable one.
   */
  const routePanel = useRoutePanel();
  /* The one viewport question the application asks (§3.2); here it decides
     whether `?panel=` describes anything at all — see the effect below. */
  const compactViewport = useCompactViewport();
  /*
   * `?from=` is a property of *this* visit to the report, so every move that
   * stays on this track has to hand it back explicitly — `go` clears whatever it
   * is not given (#1191 §1.3). Crossing to another track drops it, because the
   * return path belonged to the track being left.
   */
  const routeFrom = useRouteFrom() ?? undefined;
  const cardRegistry = cardRuntime.registry;
  // `showTrack: false` — on a track's own page the track's name is the page title,
  // so repeating it on every row is one column spent saying nothing.
  // The same predicate the planner entry resolves by (`INV-CARD-182`), imported
  // rather than copied: hiding the card from CARDS and giving it a drawer are
  // one decision, and two hand-written copies would drift apart silently.
  const plannerCard = cards.find((card) => card.kind === 'codex' && isPlannerHarnessPayload(card.payload));
  const registry = useConversationRegistry();
  /*
   * The track's assistant conversations (#1189). Its own endpoint, its own list;
   * the planner card is deliberately not in it — the server's list predicate is
   * `role == Assistant` — so the row for it is injected below.
   */
  const conversationsQuery = useQuery(trackConversationsQueryOptions(transport, track.id, unauthorized));
  const assistantRows = useMemo(() => conversationsQuery.data ?? [], [conversationsQuery.data]);
  const trackTitle = trackDisplayTitle(track.title);
  /*
   * The track's opening conversation, derived from the planner card rather than
   * listed — it is the one row on this route the server does not send.
   *
   * `updatedAt` comes from the card's own `updated_at`. That is the same
   * *quantity* the listed rows carry — epoch milliseconds off the same clock —
   * which is what the one ordering `ChatList` applies (`byRecency`) needs. It
   * is **not** the same column: a listed row reads
   * `COALESCE(worker_sessions.updated_at_ms, cards.updated_at)`, so it usually
   * reports its session's last activity while this row reports when the card
   * itself was last written. Both are "when something last happened to this
   * conversation" to within the accuracy this list claims, and neither moves
   * per turn — the drawer's own reading is the one that does, and the registry
   * keeps the later of the two (`useConversationStore`, the batch remember).
   *
   * `state` is null: no endpoint reports this card's session state to this
   * route, and `null` says "nothing is known to be happening", which is the
   * honest reading. The row picks up the live phase the moment it is opened —
   * that is the one row `useConversationStore` replaces in place.
   */
  const plannerRow = useMemo<Conversation | null>(() => plannerCard === undefined ? null : {
    id: plannerCard.id,
    trackId: track.id,
    trackTitle,
    title: plannerCard.title,
    kind: 'shared-spec',
    state: null,
    updatedAt: plannerCard.updated_at,
  }, [plannerCard, track.id, trackTitle]);
  /*
   * ── Redeeming "open the planner conversation of the track I just created" ────
   *
   * The intent rides on the history entry the create navigated to
   * (`usePlannerOpenIntent`), so `armed` is already "this track, this visit": no
   * other route body can see it, and there is no global slot for one of them
   * to clear out from under another. What is left here is the half only this
   * component knows — which card the intent names. `POST /api/tracks` answers
   * with a `Track`, and the planner card's id exists only once the detail has
   * landed, which is here.
   *
   * `disarm()` before the open, unconditionally: a track with no planner card has
   * nothing to open, and an intent left armed on this entry would fire on the
   * next visit to it (the Back button reaches one).
   *
   * `focusComposer` is what makes the landing complete: the sentence that made
   * this track was delivered into that conversation (#1299), so the caret
   * belongs where the reply to it will be read and answered.
   */
  const plannerOpenIntent = usePlannerOpenIntent(track.id);
  useEffect(() => {
    if (!plannerOpenIntent.armed) return;
    /* Read before the disarm, which is what makes this one-shot: the message
       is struck off the entry with the marker. */
    const firstMessage = plannerOpenIntent.message;
    plannerOpenIntent.disarm();
    if (plannerCard === undefined) return;
    registry.requestOpen(plannerCard.id, { focusComposer: true });
    /* And this is where the sentence that made the track finally has a card to
       put it on (#1449): `POST /api/tracks` answers with a Track, so this is
       the first moment the planner card has an id to key the slot by. */
    if (firstMessage !== null) registry.noteCreateEcho(plannerCard.id, firstMessage);
  }, [registry, plannerCard, plannerOpenIntent]);
  /* Every row carries the track's title, so a row that reaches Today can say
     where it is. On this page `showTrack: false` hides it again.
     *
     * Unconditionally, and *before* the planner row is considered: whether this
     * track happens to have a planner card has nothing to do with whether its
     * assistant rows know where they live, and while the two were one
     * expression the `plannerRow === null` arm returned the rows untouched. A
     * reader who had only ever visited tracks without a planner card then saw a
     * Today list of rows reading `Assistant` with no track named on any of
     * them — the tracks that most need the `+` (§5.3) losing the label first. */
  const placedRows = useMemo(
    () => assistantRows.map((row) => ({ ...row, trackTitle })),
    [assistantRows, trackTitle],
  );
  const rows = useMemo<readonly Conversation[]>(
    () => plannerRow === null ? placedRows : [plannerRow, ...placedRows],
    [placedRows, plannerRow],
  );
  /*
   * `'rows'`, unconditionally — no longer `'card'` when a planner card exists and
   * `'elsewhere'` when it does not (§5.3).
   *
   * The branch that is gone took the `+` away from exactly the tracks that need
   * it most: a track with no planner card had no conversation at all and no way to
   * start one. The list being empty is a state this panel already renders, and
   * an empty list with a `+` over it is the whole feature.
   */
  const chat = useConversationPanel(
    transport,
    unauthorized,
    {
      scopeId: track.id,
      rows,
      /* Unlike an area's, these rows are on a track the reader can be sent to —
         this very route — so Today may hold and open them. The store checks
         each row's `trackId` against this, so a row from anywhere else is not
         remembered whatever put it in the list. */
      rememberOn: track.id,
      derivedCardId: (idempotencyKey) => trackConversationCardId(track.id, idempotencyKey),
      scopeOf: (conversationId) => {
        /* The planner row first: it is not in `assistantRows`, and without this
           arm the one conversation a track has always had would stop opening. */
        if (plannerCard !== undefined && conversationId === plannerCard.id) {
          return {
            id: track.id, title: trackTitle, cardId: plannerCard.id,
            cardTitle: plannerCard.title, updatedAt: plannerCard.updated_at,
            kind: 'shared-spec', state: null,
          };
        }
        const row = assistantRows.find((candidate) => candidate.id === conversationId);
        /*
         * `id: row.trackId` — the row's own track, never `track.id`.
         *
         * This is the line the whole `rememberOn` defence rests on. `scope.id`
         * becomes `conversation.trackId` in the store, which is what
         * `conversation.trackId !== rememberOn` compares against the track this
         * route claimed. Written `id: track.id` it would be a tautology — every
         * open row would pass, whatever track it really belongs to — and no
         * existing test would notice: the fixtures list rows of this track, so
         * the two values are equal in every green case. The comparison is only
         * a comparison because this side is the row's.
         */
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
  /*
   * The fallback clear, for the one case the effects on both sides leave open.
   *
   * `TrackRoute` clears a request whose card this track does not have, and the
   * panel consumes one whose row it does. What neither covers is a request for
   * a card this track *does* have while the list that would open it could not be
   * read: the panel is right to keep waiting, and the wait would never end. The
   * reader then walks into this track some other day and the drawer springs open
   * for a conversation they asked about once.
   *
   * So: the read is over, it failed, and the id is not among whatever rows did
   * arrive. Not "the read failed", which would also throw away a perfectly
   * openable planner row.
   *
   * A fail-safe with no producer since #1341, for the same reason and on the
   * same terms as the clear in `TrackRoute`: the only request this route can
   * receive today names its planner card, which is in `rows` whatever the
   * conversation list did.
   */
  useEffect(() => {
    const requestedOpenId = registry.requestedOpenId;
    if (requestedOpenId === null || !conversationsQuery.isError) return;
    if (rows.some((row) => row.id === requestedOpenId)) return;
    registry.clearOpenRequest();
  }, [conversationsQuery.isError, registry, rows]);
  /*
   * The runtime half of the TASKS panel.
   *
   * Deliberately keyed `['track-report', trackId]`, which is the key
   * `core/events/invalidation-plan` has always planned for every `task.*`
   * event and for `track.report_edited` — naming it anything else would have
   * meant a second, hand-rolled refresh path for a panel the event plan
   * already knew how to keep live.
   */
  /* Read before the query, not inside the join below it: the poll's own
     interval is a question about the rows this report can produce, so the
     declarations have to be in hand when the query options are built. */
  const report = useMemo(() => readTrackReport(cards), [cards]);
  const reportBlocks = report?.blocks ?? null;
  const verdictsQuery = useQuery(
    trackTaskVerdictsQueryOptions(transport, track.id, unauthorized, reportBlocks),
  );
  const verdicts = verdictsQuery.data;
  const { outline, tasks: joinedTasks } = useMemo(() => ({
    outline: deriveReportOutline(reportBlocks),
    /* The declarations arrive with the track detail and the verdicts land a
       round-trip later; passing `undefined` through as "none yet" renders the
       same statusless list this panel shipped with rather than a hole. */
    tasks: deriveReportTasks(reportBlocks, verdicts),
  }), [reportBlocks, verdicts]);
  /*
   * INV-CARD-226 — the CARDS module lists cards that have a surface. `planner` and
   * `track-report` resolve to headless adapters and are dropped; anything no
   * adapter claimed stays, because an unlisted card is worse than an
   * unrecognised one. Both branches carry `originalIndex`, bound before the
   * filter, and the merge re-sorts on it so the panel keeps the wire order the
   * kernel sent — a post-filter index here would address the wrong card the
   * moment remove/action callbacks land (S2).
   */
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
        /* The kernel's bit, carried straight through: the board decides whether
           to draw a × from this, and the CARDS panel reads the same field off
           the same wire row, so the two surfaces cannot disagree about which
           cards are the kernel's. */
        deletable: slot.wire.deletable,
      }));
  }, [cardRegistry, cards]);
  const inputNotifications = useMemo(
    () => cardInputNotifications(cards, overlays),
    [cards, overlays],
  );
  const conversationNotificationCardIds = useMemo(
    () => new Set(cards
      .filter((card) => card.kind === 'codex'
        && (isPlannerHarnessPayload(card.payload) || isAssistantHarnessPayload(card.payload)))
      .map((card) => card.id)),
    [cards],
  );
  /*
   * A task row may only offer its worker card when that card can actually be
   * opened — and "openable" is asked of the *registry*, through the very list
   * the board draws, never of a hardcoded set of worker kinds.
   *
   * The kernel dispatches `codex`, `claude` and `terminal` workers, and this
   * build's registry can draw all three — `codex` was the standing exception
   * until `CODEX_CARD_ENTRY` landed, and nothing here changed when it did: the
   * id simply started resolving, which is the point of asking the registry.
   * What the filter still catches is any worker card whose kind no entry claims
   * — a kernel newer than this bundle stamping one is the live case. Such a card
   * is `unknown`: it is not in `gridItems`, `knownCard` below is false for it,
   * and the effect under this line bounces `?card=` straight back off the URL.
   * A row that clicked there would land the reader nowhere and lose the reveal
   * it used to have. Filtering here rather than teaching `TrackPage` about the
   * registry keeps the panel a pure renderer.
   */
  const tasks = useMemo(() => {
    const openable = new Set(gridItems.map((item) => item.card.id));
    return joinedTasks.map((task) => (task.workerCardId === null || openable.has(task.workerCardId)
      ? task
      : { ...task, workerCardId: null }));
  }, [gridItems, joinedTasks]);
  const knownCard = requestedCardId !== null
    && gridItems.some((item) => item.card.id === requestedCardId);
  useEffect(() => {
    if (requestedCardId === null || knownCard) return;
    // Bouncing an unopenable `?card=` must drop *only* that parameter: the
    // panel, the return surface and the block anchor describe where the reader
    // is, not which card they asked for. Clearing them here was a regression
    // this route shipped with.
    goSameTrack(track.id, { card: undefined }, { replace: true });
  }, [goSameTrack, knownCard, requestedCardId, track.id]);
  /*
   * `?panel=` is a *compact* concept, and above the breakpoint it does not just
   * sit there unused — it takes the desktop panel down with it.
   *
   * `TrackPage` derives `mobilePanelOpen` from this prop alone and puts `inert` +
   * `aria-hidden` on the desktop panel surface while it is open; on desktop the
   * mobile list is `display: none`. A shared `?panel=cards` link opened on a
   * laptop therefore rendered a panel that is fully visible and completely
   * unreachable by keyboard or screen reader — nothing to see, nothing to fix
   * from the page.
   *
   * Two halves, and neither is sufficient. The URL is corrected here so it stops
   * describing a state this viewport cannot be in — a `replace`, because
   * widening the window is not a place the reader can go Back to — and the
   * *injection* below is gated on the viewport as well, because on a cold start
   * this effect has not run yet when the first paint happens.
   */
  useEffect(() => {
    if (compactViewport || routePanel === null) return;
    goSameTrack(track.id, { panel: undefined }, { replace: true });
  }, [compactViewport, goSameTrack, routePanel, track.id]);
  /*
   * One confirm for both delete gestures.
   *
   * The CARDS panel row and the card's own head on the board are two entry
   * points to the same irreversible act on the same row, so they share one
   * dialog and one copy (INV-DUP-010) rather than each growing their own.
   *
   * Nothing here navigates on success. The delete drops the row from the
   * detail cache, `gridItems` loses it on the next render, and the
   * `knownCard` effect above — which exists for cards this build's registry
   * cannot draw — bounces `?card=<deleted>` off the URL for the same reason it
   * always did. A `go()` here would be a second, racing route write.
   */
  const cardDeletion = useDeleteConfirm(
    (cardId, signal) => trackMutations.removeCard(track.id, cardId, signal),
  );

  /*
   * ── Adding a card ─────────────────────────────────────────────────────────
   *
   * The menu is the registry's own list (`cardAddMenuEntries`), so this route
   * never decides *what* can be created — only *how*, which is the one part it
   * is allowed to know: which endpoint a kind takes is a fact about the kernel,
   * and `systems/cards` sits below `app/**` and holds no transport.
   *
   * Two doors, and the kind's own create strategy says which one it takes:
   *
   *   - `atomic` — the kernel writes the row and spawns a runtime in one call,
   *     through an endpoint named after the kind (`terminal-cards`,
   *     `codex-cards`). A worker card has a daemon behind it, so there is no
   *     generic form of this create and the mapping below is explicit.
   *   - `generic` — the row is all there is (`file-viewer`), so it goes through
   *     `POST /api/tracks/:id/cards` with the kind's own `buildPayload`, and the
   *     kind on the wire is the entry's `claim`, which is why `registerCard`
   *     refuses a generic entry that does not claim one exactly.
   *
   * A kind whose strategy this table does not handle throws rather than falling
   * back to a guess: a silent "create nothing" on a menu item the reader picked
   * is the worst of the three outcomes.
   */
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
  /*
   * A failed create has to be sayable with no dialog on screen.
   *
   * A kind with no fields (`terminal`) never opens one — `pickCardKind` posts on
   * the spot — so a message that only `NewCardForm` renders is a message the
   * reader of that path never sees: the `+` menu closes and nothing happens at
   * all. Routing it through the same `useOperationFeedback` the delete path uses
   * gives it a route-level surface, and the dialog keeps rendering the very same
   * `error` inline while it is open, so the two cannot disagree about what went
   * wrong (and it is never printed twice — see the render).
   */
  const cardCreateFeedback = useOperationFeedback();
  const newCardFieldRef = useRef<HTMLInputElement | null>(null);
  /*
   * A create that lands after the reader has left must not steer them.
   *
   * `submitNewCard` navigates on success, and the post outlives this route body
   * whenever the reader moves on while it is in flight — the navigation would
   * then yank them back to a track they deliberately left. The shape is the one
   * `useDeleteConfirm` already uses (INV-CONFIRM-001): one `AbortController`
   * per attempt, aborted when this body unmounts, and read before the
   * navigation and before every state write. The card is still created — the
   * kernel's write is not the reader's problem — but nothing here acts on it.
   */
  const activeCardCreate = useRef<AbortController | null>(null);
  useEffect(() => () => { activeCardCreate.current?.abort(); }, []);

  const createCardOfKind = async (entry: CardAddMenuEntry, values: NewCardValues) => {
    /* Empty is absent, not `""`. The kernel reads an empty `cwd` as "no
       directory given" for codex but an empty `title` is a real, blank title —
       so the drop happens here, once, rather than in each branch. */
    const given = (key: string): string | undefined => {
      const value = (values[key] ?? '').trim();
      return value === '' ? undefined : value;
    };
    const title = given('title');
    /* Read at click time from `<html data-theme>` rather than through
       `useTheme()`: subscribing here would re-render the track subtree on every
       theme toggle and remount any live terminal under it (#177). */
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

  /* Opening the new card is the point of creating one, so the create navigates
     to it — the same landing `onOpenCard` gives a row that already exists. */
  const submitNewCard = (entry: CardAddMenuEntry, values: NewCardValues) => {
    /* Landing on the card you just made is a gesture, and only the newest
       gesture may own it. A second create supersedes the first, so the first is
       aborted here rather than merely forgotten — forgetting it left it holding
       a live `goSameTrack` that steers a reader who has since gone elsewhere.
       The superseded attempt's card is still created server-side; the abort only
       stops it steering, and stops its `finally` clearing a busy state that now
       belongs to the newer attempt. Unmount still aborts whatever is current. */
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
        /* Only the attempt that still owns the busy state may clear it. Both
           ways of losing ownership — unmount and being superseded above —
           abort, so `aborted` is the whole test; an identity check against
           `activeCardCreate.current` would never fire on a live controller. */
        if (controller.signal.aborted) return;
        activeCardCreate.current = null;
        setCreatingCard(false);
      });
  };

  /* A kind with nothing to ask is created on the spot; one with fields opens
     the form. The menu itself never creates anything — see `AddCardMenu`. */
  const pickCardKind = (entry: CardAddMenuEntry) => {
    cardCreateFeedback.clear();
    if (entry.fields.length === 0) submitNewCard(entry, {});
    else setCardDraft(entry);
  };
  const backlinksQuery = useQuery(trackBacklinksQueryOptions(transport, track.id, unauthorized));
  const backlinks = backlinksQuery.data;

  /*
   * A `neige://wave/…` citation. Same track — the common case, since a report
   * mostly cites its own sections — reveals immediately so activating an
   * unchanged hash still flashes the destination, then records that destination
   * in the URL. The route body is keyed by track id, so the hash update preserves
   * the document. Another track is a real navigation carrying the same hash.
   */
  const arrivalAnchorId = useRouteHash();
  const openReportLink = (target: ReportLinkTarget) => {
    if (target.trackId === track.id) {
      if (target.blockId !== null) revealReportAnchor(target.blockId);
      // Same track: landing on a block is a move *within* the report, so the
      // return surface survives and the panel closes (the document is now what
      // the reader is looking at).
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

  return (
    <>
    <TrackStage>
    <TrackPage
      track={track}
      canResumeTrack={canResumeTrack}
      cards={panelCards}
      /* Derived from the report's own blocks, so the panel and the document
         cannot disagree about what tasks exist. */
      tasks={tasks}
      outlineItems={outline}
      /* Tasks and the mobile Outline share one anchor landing. The URL carries
         it too, so the reader can hand the destination to somebody else. */
      onOpenTask={openReportAnchor}
      onOpenOutline={openReportAnchor}
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
      /* Gated on the viewport, not only on the card: the effect above cannot
         have run yet on a desktop cold start, and one render with the panel
         "open" is one render with the desktop panel `inert`. */
      panel={renderedMobilePanel(routePanel, {
        compact: compactViewport,
        overlayOpen: requestedCardId !== null || rawRequestedFilePath !== null,
      })}
      onOpenPanel={(kind) => { openPanel(track.id, kind); }}
      onClosePanel={() => { closePanel(track.id); }}
      /* `?from=` is the whole memory of how the reader got here; absent means
         Pages, which is the default this route shipped with (§1.2). The area to
         return to is the track's own, not a stored restore id. */
      mobileBackLabel={routeFrom === 'area' ? 'Tracks' : 'Pages'}
      onMobileBack={() => {
        if (routeFrom === 'area') openMobileSection('areas', track.areaId);
        else openMobileSection('pages');
      }}
      report={<ReportDocument
        report={report}
        taskVerdicts={verdicts}
        rail={<ReportOutline items={outline} />}
        backlinkCounts={backlinks === undefined ? undefined : backlinkCountsByBlock(backlinks.backlinks)}
        onOpenLink={openReportLink}
        onOpenFileLink={openReportFile}
        fileRoot={track.cwd}
        arrivalAnchorId={arrivalAnchorId}
        /*
          #1211 S2 — "Nothing written here yet." described a missing artefact,
          and it read as an omission the reader had made. It is not one: a track
          now starts with no name and no words in it *by design*, and the true
          state of this page on arrival is "this track has not taken shape yet
          — say the first thing". The lead says that, and the first hint names
          the one action that changes it, which is the conversation already
          open beside it.
        */
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
    {/* Only while the dialog is closed: `NewCardForm` renders the same `error`
        inline, and a fieldless kind never opens the dialog at all — which is
        precisely the path that had no surface of any kind. */}
    {cardDraft === null && <OperationFeedback feedback={cardCreateFeedback} />}
    {chat.drawer}
    </>
  );
}
