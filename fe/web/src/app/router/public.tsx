// Code-based TanStack Router setup.
//
// The whole tree is built inside a factory: `createRoute`/`createRouter` at
// module scope would be module runtime state, and injecting the transport and
// the QueryClient is what lets a test drive a real router without touching a
// module singleton.
//
// This module is also the composition point the layering forbids anywhere
// else: `features/cove` may not import `features/wave`, so the cove route is
// where `<CovePage>` and `<WaveList>` are put together.

import {
  createRootRoute, createRoute, createRouter, type AnyRoute,
} from '@tanstack/react-router';
import { useEffect, useMemo, useRef } from 'react';
import { useInfiniteQuery, useQuery, type QueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { coveOf, type Cove } from '../../../../core/domain/cove.ts';
import {
  toWave, waveActivityFrom, waveDisplayTitle, type Wave, type WaveDetailWire,
} from '../../../../core/domain/wave.ts';
import type {
  BoardHostItem, CardAddMenuEntry, CardHost, CardRegistry,
} from '../../systems/cards/public.js';
import {
  cardAddMenuEntries, isAssistantHarnessPayload, isSpecHarnessPayload, partitionWaveCards,
} from '../../systems/cards/public.js';
import { mintIdempotencyKey } from './idempotency-key.ts';
import { CovePage } from '../../features/cove/page/public.tsx';
import { SettingsPage, type ThemeMode as SettingsThemeMode } from '../../features/settings/public.tsx';
import { TemplateEditorPage, TemplateListPage } from '../../features/settings/templates.tsx';
import { TodayPage } from '../../features/today/public.tsx';
import { WaveList } from '../../features/wave/list/public.tsx';
import { WaveRow } from '../../features/wave/row/public.tsx';
import { WavePage } from '../../features/wave/page/public.tsx';
import { CardGridOverlay, WaveStage } from '../../features/wave/grid/public.tsx';
import { AddCardMenu, NewCardForm, type NewCardValues } from '../../features/wave/new-card/public.tsx';
import { ChatList } from '../../features/chat/list/public.tsx';
import {
  ChatComposer, ChatFooterError, ChatFooterNotice, ChatFooterRemedy, ChatThread,
} from '../../features/chat/thread/public.tsx';
import { ReportBacklinks } from '../../features/report/backlinks/public.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ReportEmpty } from '../../features/report/empty/public.tsx';
import { ReportOutline } from '../../features/report/outline/public.tsx';
import { revealReportAnchor } from '../../features/report/anchor/public.ts';
import {
  backlinkCountsByBlock, deriveReportOutline, deriveReportTasks, readWaveReport, type ReportLinkTarget,
} from '../../../../core/domain/report.ts';
import {
  buildTranscript, conversationName, conversationNameFrom, CONVERSATION_STATE_SOURCE,
  coveConversationFailure, COVE_CONVERSATION_TEXT_MAX, harnessItemToTurn, mergeTranscript, reconcileUserEchoes,
  waveConversationCardId, coveConversationCardId,
  type Conversation, type ConversationKind, type ConversationState, type ConversationTurn,
  type TranscriptEntry,
} from '../../../../core/domain/conversation.ts';
import { ConfirmDialog, Dialog } from '../../ui/dialog/public.tsx';
import { createDirectoryLister } from '../providers/directory.ts';
import { DELETE_CARD_COPY, DELETE_WAVE_COPY } from '../../ui/confirm-dialog/copy.ts';
import { OperationFeedback, useDeleteConfirm, useOperationFeedback } from '../../ui/operation-feedback/public.tsx';
import { Drawer } from '../../ui/drawer/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { PanelAction } from '../../ui/panel-card/public.tsx';
import { useReducer, useState } from '../../ui/state/public.ts';
import {
  ApiError, coveConversationsQueryOptions, harnessItemsQueryOptions, prefetchCoveList,
  settingsQueryOptions, specRunQueryOptions, useCoveConversationMutations, useCoveMutations,
  useSettingsMutation, useSpecMutations, useWaveConversationMutations, useWaveMutations,
  useWaveTemplateMutation, useWaveTemplates, useWorkspace,
  todayLaunchpadQueryOptions, waveBacklinksQueryOptions, waveConversationsQueryOptions,
  waveDetailQueryOptions, waveTaskVerdictsQueryOptions,
} from '../providers/queries.ts';
import { AppShell, useOpenMobileSection, useRequestNewWave } from '../shell/public.tsx';
import { useTheme } from '../theme/public.tsx';
import { ConversationProvider, useConversationRegistry } from '../conversations/public.tsx';
import {
  renderedMobilePanel,
  useGo, useGoSameWave, useRouteCardId, useRouteFrom, useRouteHash, useRoutePanel, useRouteParam,
  useSpecOpenIntent, useWavePanelNavigation, validateWaveSearch, type WaveSearch,
} from './navigation.ts';
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
  hasEarlier: boolean;
  loadingEarlier: boolean;
  historyError: string | null;
  actionError: string | null;
  start: () => Conversation | null;
  send: (conversationId: string, text: string) => void;
  interrupt: () => void;
  loadEarlier: () => void;
}>;

/**
 * Where a route's conversation list comes from.
 *
 * `'all'` and `'waves'` read the session registry — conversations this tab has
 * opened. `'rows'` is the opposite: the server sends the list, so the registry
 * is not consulted for it.
 *
 * `'waves'` has no construction site left: the cove route filtered the registry
 * by its waves before it listed conversations from the server (#1098), and
 * Today is the only registry-backed surface now, with `'all'`. It is left
 * standing rather than deleted here because deleting it is not one line — the
 * same sweep takes `ConversationPanelSource`'s `'card'` arm, `store.start` and
 * the `scope` arm of the open-request consume with it, which is a wider edit
 * than a review fix should carry.
 *
 * What that sweep would **not** change is behaviour, and an earlier version of
 * this note claimed otherwise. The live Today → wave path runs through the
 * `rows !== null` branch of that consume (`useConversationPanel`); the `scope`
 * branch below it is only reachable when the source is `'card'`, which nothing
 * selects. So the reason to defer is size and blast radius, not risk to the
 * path this slice is about. Recorded here so nobody reads a live decision into
 * an arm nothing selects (#1189 review).
 */
type ConversationListIntent = Readonly<
  { kind: 'all' } | { kind: 'waves'; waveIds: readonly string[] }
>;

type ConversationRouteIntent =
  | ConversationListIntent
  | Readonly<{
    kind: 'rows';
    rows: readonly Conversation[];
    /**
     * The wave these rows may be *sent to*, or null when there is none.
     *
     * This is what decides whether a `'rows'` route writes its list back into
     * the session registry, and it is a wave id rather than a boolean so the
     * decision can be enforced here rather than trusted (§5.1).
     *
     * A cove route passes null: its rows live on the cove's hidden chat wave,
     * Today navigates to `conversation.waveId` when a row is opened, and
     * remembering them would walk the reader into a wave that is deliberately
     * on no list. A wave route passes its own wave id: its rows are on a real,
     * navigable wave, and they are the *only* place an assistant conversation
     * exists — without this they could never reach Today at all.
     *
     * It lives on the route intent and not on `ConversationPanelSource` because
     * the store is where the decision is made and the store is handed only
     * `scope` and this value; a field on the source would be invisible here.
     */
    rememberOn: string | null;
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
  waveId: string;
  waveTitle: string | undefined;
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
  facts: ConversationFacts, turns: readonly ConversationTurn[],
): Conversation {
  return {
    id: facts.cardId, waveId: facts.waveId,
    /* Absent, not `''`: a cove conversation's wave is hidden and has no title
       to show. `ChatList` renders the difference; `''` would render a blank. */
    ...(facts.waveTitle === undefined ? {} : { waveTitle: facts.waveTitle }),
    title: facts.cardTitle
      ?? conversationNameFrom(turns.find((turn) => turn.author === 'you')?.text ?? ''),
    kind: facts.kind,
    /* A server-listed conversation's state is the server's to report —
       `run_status_for` writes `turn_pending`, never `running`, for a headless
       harness, and everything outside the four live states arrives as `null`.
       The local phase still wins while a turn is in flight, because the list
       would otherwise sit on the state the last fetch happened to catch.
       Which kinds those are is a total table (`CONVERSATION_STATE_SOURCE`) and
       not a `=== 'shared-chat'` test: this branch is silent, and a new kind
       falling into the `else` would swap the server's reading for an invented
       `'idle'` with nothing to notice it. */
    state: CONVERSATION_STATE_SOURCE[facts.kind] === 'server'
      ? (facts.working ? 'turn_pending' : facts.state)
      : (facts.working ? 'running' : 'idle'),
    updatedAt: turns.at(-1)?.atMs ?? facts.fallbackUpdatedAt,
    turns: turns.length,
  };
}

export function useConversationStore(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
  scope: SpecConversationScope | null,
  routeIntent: ConversationRouteIntent,
): ConversationStore {
  const registry = useConversationRegistry();
  const cardId = scope?.cardId ?? '';
  const waveId = scope?.id;
  const waveTitle = scope?.title;
  const cardTitle = scope?.cardTitle;
  const scopeUpdatedAt = scope?.updatedAt;
  const scopeKind = scope?.kind ?? 'shared-spec';
  const scopeState = scope?.state ?? null;
  const serverRows = routeIntent.kind === 'rows' ? routeIntent.rows : null;
  const rememberOn = routeIntent.kind === 'rows' ? routeIntent.rememberOn : null;
  const history = useInfiniteQuery({
    ...harnessItemsQueryOptions(transport, cardId, unauthorized), enabled: scope !== null,
  });
  const run = useQuery({ ...specRunQueryOptions(transport, cardId, unauthorized), enabled: scope !== null });
  const mutations = useSpecMutations(transport, cardId, unauthorized);
  const [echoes, setEchoes] = useState<readonly ConversationTurn[]>([]);
  /**
   * The echo whose `POST /spec/input` has not been answered yet, if any.
   *
   * One id and not a set, and that is a claim about reachability rather than
   * about this line: a second unanswered echo would make `confirmedEchoes`
   * report the first one as confirmed, which is exactly the false fact the
   * registry must never be told (`durableConversation`).
   *
   * What holds it to one is `sendingRef` **plus** `activeSend` below. On its
   * own `sendingRef` does not: the `cardId` effect clears it so a second
   * conversation can be written in while the first one's POST is still out, and
   * an in-flight request that cleared it again on the way out — as this one
   * unconditionally did — would re-open the composer over a *new* conversation
   * whose own echo was still unanswered. Two unanswered echoes, one slot. The
   * settle handlers below therefore touch the send state only while they are
   * still the active request, which is what makes "at most one" true rather
   * than merely intended.
   *
   * "At most one" is a statement about **this mounted store**, and only about
   * it. A store that unmounts with a send still out leaves that request holding
   * an echo nobody here can see; the store mounted next has its own slot, and
   * the two can be unanswered at the same time. That is why echo ids carry a
   * per-instance nonce (`echoNonce`) — the shared registry is the one place
   * those two lifetimes meet.
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
  /**
   * Every echo id this store has minted, which is how a write-through tells an
   * echo of its own apart from a user message the server sent back.
   *
   * Ids are `echo-<nonce>-N` off a counter that is never reset, so they stay
   * unique across conversation switches for the life of the store.
   */
  const mintedEchoIds = useRef(new Set<string>());
  const seq = useRef(0);
  /**
   * What makes an echo id unique beyond this store instance.
   *
   * `seq` and `mintedEchoIds` die with the store; the registry does not — it
   * lives on `ConversationProvider` at the root and outlives every route. So a
   * counter alone is only unique *within one mounted instance*: leave the route
   * with a send still in flight and come back, and the new store starts at
   * `echo-1` again while the old closure is still holding an `echo-1` of its
   * own. Both write through on success, and two different messages enter the
   * shared registry under one id — a duplicate React key in the transcript, and
   * a `mintedEchoIds` answer given about the wrong message.
   *
   * A per-instance nonce is the bounded fix: ids stay collision-free across
   * mounts without moving ownership of minting up to the provider. Minted off
   * `getRandomValues` (via `mintIdempotencyKey`) because the app is served over
   * plain http on the LAN, where `crypto.randomUUID` does not exist.
   */
  const [echoNonce] = useState(mintIdempotencyKey);
  const items = useMemo(() => (history.data?.pages ?? []).flat(), [history.data]);
  const serverTurns = useMemo(() => [...items]
    .sort((left, right) => left.id - right.id).flatMap((item) => {
      const turn = harnessItemToTurn(item);
      return turn === null ? [] : [turn];
    }), [items]);
  /* Actions are read from the same rows, so they cannot disagree with the
     messages about what happened or when (`buildTranscript`). */
  const serverEntries = useMemo(() => buildTranscript(items), [items]);
  useEffect(() => {
    setEchoes((current) => reconcileUserEchoes(serverTurns, current));
  }, [serverTurns]);
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
  const transcript = useMemo(() => mergeTranscript(serverEntries, echoes), [echoes, serverEntries]);
  const confirmedTranscript = useMemo(
    () => mergeTranscript(serverEntries, confirmedEchoes), [confirmedEchoes, serverEntries],
  );
  const phase = run.data?.phase ?? null;
  const working = phase === 'issuing_turn' || phase === 'turn_running';
  const stopping = phase === 'issuing_interrupt' || interruptPending;
  const facts = useMemo<ConversationFacts | null>(() => waveId === undefined ? null : {
    cardId, waveId, waveTitle, cardTitle: cardTitle ?? null, kind: scopeKind,
    state: scopeState, working, fallbackUpdatedAt: scopeUpdatedAt ?? 0,
  }, [cardId, cardTitle, scopeKind, scopeState, scopeUpdatedAt, waveId, waveTitle, working]);
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
   * has no `forget`, and Today reads it for the life of the tab. So the one
   * thing that may never enter it is a fact that is not yet a fact.
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
    /*
     * A server-listed conversation is not remembered *here*.
     *
     * The registry exists so a conversation stays visible on routes that cannot
     * fetch it — which is exactly what a `'rows'` route can do for itself. What
     * remembering would add on a cove route is a leak: those rows live on the
     * cove's hidden chat wave, and Today lists everything the registry holds and
     * navigates to `conversation.waveId` when a row is opened, which would walk
     * the reader into the hidden wave. Today has no second filter.
     *
     * A `'rows'` route that named a wave (`rememberOn`) is the exception, and
     * it carries that defence itself: only the named wave's rows are written,
     * here and in the effect below.
     */
    if (serverRows !== null && (rememberOn === null || durableConversation.waveId !== rememberOn)) return;
    /* Remember the transcript so reopening the conversation preserves its
       activity lines and looks identical to the route the user just left — the
       confirmed one, for the reason `durableConversation` gives: a message that
       may still fail is not part of what this conversation *is*. */
    registry.remember(durableConversation, confirmedTranscript);
  }, [confirmedTranscript, durableConversation, registry, rememberOn, serverRows]);
  useEffect(() => {
    /*
     * A `'rows'` route that named a wave remembers **every** row it lists.
     *
     * Not only the open one, and not only on open. An assistant conversation
     * exists nowhere but in this list — no other surface fetches it — so a
     * registry that only learned about opened rows would leave Today permanently
     * without them, and the whole Today → wave path below would be dead code for
     * the one kind of conversation it was written for (§5.1).
     *
     * `rememberOn` is compared against each row rather than merely consulted:
     * Today navigates to `conversation.waveId`, so a row belonging to some other
     * wave — or to a hidden one — would be a link into a place this route never
     * claimed. Remembering exactly the rows of the named wave is the defence,
     * held here rather than downstream.
     *
     * Turns are carried over from whatever the registry already holds, never
     * reset: the open row is remembered with its full transcript by the effect
     * above, and writing `[]` here would erase it on the next render.
     */
    if (serverRows === null || rememberOn === null) return;
    for (const row of serverRows) {
      if (row.waveId !== rememberOn) continue;
      /* The open row belongs to the effect above, which knows its transcript
         and its live name. Writing the plain row over that here would undo it
         on every render, and the two effects would then take turns rewriting
         one entry for as long as the drawer stayed open. */
      if (row.id === conversation?.id) continue;
      /* A turn count this tab really read is not unread by a list that does not
         send one. The server will not count turns (`WaveConversationSummary`),
         so a row always arrives with `turns` absent; writing that over an entry
         the drawer counted would make a conversation lose its count on Today
         the moment its own wave was visited again. The transcript is carried
         over for the same reason and by the same rule.

         And so is the **name**, which is that rule a third time and was the one
         omission: an assistant card is minted `title: None`
         (`wave_conversations.rs`) and nothing backfills it, so `row.title` is
         permanently null on the wire. The name a reader sees is derived by the
         effect above from the conversation's first message. The moment the
         drawer closes — or opens on another row — this effect stops skipping
         that row, and a plain `{...row}` would put the null back: Today would
         fall from the derived name to the bare kind label `Assistant`. Only
         the absent direction is carried — a title the server *does* send (the
         cove list's, and any future backfill) is the server's to change and
         wins, exactly as `turns` does. */
      /* `updatedAt` never goes backwards, which is the same rule once more.
         A row's time is whatever column produced it — the listed rows read
         `COALESCE(worker_sessions.updated_at_ms, cards.updated_at)`, the
         injected spec row reads the card's `updated_at`, and neither moves
         when a turn is added to a conversation the drawer is reading. The
         drawer *does* know that time (`turns.at(-1)?.atMs`) and wrote it here.
         Taking the later of the two keeps a conversation you have just been
         talking in at the top of Today, instead of sinking it back to whenever
         its card row was last touched. */
      const known = registry.conversations.find((candidate) => candidate.id === row.id);
      registry.remember(
        {
          ...row,
          ...(known?.turns === undefined ? {} : { turns: known.turns }),
          ...(row.title === null && known?.title != null ? { title: known.title } : {}),
          updatedAt: Math.max(row.updatedAt, known?.updatedAt ?? 0),
        },
        registry.turnsOf(row.id),
      );
    }
  }, [conversation?.id, registry, rememberOn, serverRows]);

  const allConversations = conversation === null
    ? registry.conversations
    : [...registry.conversations.filter(({ id }) => id !== conversation.id), conversation];
  const waveIds = routeIntent.kind === 'waves' ? new Set(routeIntent.waveIds) : null;
  const conversations = serverRows !== null
    /* The open row is replaced in place by the live one: same id, but with the
       turns and the name this route can only know from the transcript it is
       already reading (§7 — the server has no title to send). */
    ? (conversation === null
      ? serverRows
      : serverRows.map((row) => row.id === conversation.id ? conversation : row))
    : waveIds === null
      ? allConversations
      : allConversations.filter((candidate) => waveIds.has(candidate.waveId));

  const send = (_conversationId: string, text: string) => {
    if (sendingRef.current) return;
    sendingRef.current = true;
    setSending(true);
    setActionError(null);
    seq.current += 1;
    const echo = {
      id: `echo-${echoNonce}-${seq.current}`, author: 'you' as const, text, atMs: Date.now(),
    };
    mintedEchoIds.current.add(echo.id);
    const sentTo = cardId;
    /*
     * Which server rows this conversation had *before* this message left.
     *
     * The write-through below asks whether the refresh already brought this very
     * message back, and the only rows that can answer that are the ones that
     * were not there yet. Asking the whole registry entry instead — as this did
     * — makes an older identical message answer for this one: say `ping` twice
     * and the server row for the first `ping` matches the second echo, so the
     * second send is neither appended nor counted and the reader loses a message
     * they watched being sent. `reconcileUserEchoes` pairs one-to-one only
     * within the echoes of a single call, and this call passes one echo, so it
     * cannot know the older row is already spoken for. Recording the ids here is
     * how it is told.
     */
    const turnsBefore = new Set(registry.turnsOf(sentTo).map((entry) => entry.id));
    activeSend.current = { cardId: sentTo, echoId: echo.id };
    /* Still ours to answer for. False from the moment the reader moved to
       another conversation (the `cardId` effect) or started a later send. */
    const stillActive = () => activeSend.current?.echoId === echo.id;
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
       * moment of the send. `mutations.send` resolves two refreshes after its
       * POST returned 200 (`useSpecMutations`), and either of them can land a
       * newer transcript, turn count or state in this entry first: merging into
       * `registry.conversations` and `registry.turnsOf` as captured here would
       * put the pre-send entry back and drop what just arrived. The
       * "only into an entry that already exists" check is the same story —
       * that is what keeps this on the right side of the `rememberOn` defence
       * above (on a cove route the open conversation is never remembered, so
       * there is nothing to find), and a decision made off a captured list is
       * a decision made about a list that may no longer be the one being
       * written to.
       *
       * Only the absent direction of the name, exactly as the batch remember
       * below does it — a title the server sends is the server's.
       */
      registry.updateExisting(sentTo, ({ conversation: known, turns: knownTurns }) => {
        /*
         * The refresh may already have brought this very message back.
         *
         * `POST /spec/input` is answered, the history query refetches, the
         * server's own row for this message lands, and the effect above writes
         * the transcript containing it — all before this promise settles.
         * Appending the echo then would show the reader their message twice and
         * count it twice. Matched by text through the same one-to-one rule the
         * drawer reconciles echoes with (`reconcileUserEchoes`), against the
         * rows that **arrived since the send** (`turnsBefore`) and that this
         * store did not mint itself — so a reader who genuinely sends the same
         * sentence twice still has it counted twice, and an echo of this store's
         * own is never mistaken for the server's account of it.
         */
        const serverSaid = knownTurns.filter((turn): turn is ConversationTurn =>
          turn.author === 'you' && !mintedEchoIds.current.has(turn.id) && !turnsBefore.has(turn.id));
        const recorded = reconcileUserEchoes(serverSaid, [echo]).length === 0;
        return {
          conversation: {
            ...known,
            title: known.title ?? conversationNameFrom(text),
            updatedAt: Math.max(known.updatedAt, echo.atMs),
            ...(recorded ? {} : { turns: (known.turns ?? 0) + 1 }),
          },
          turns: recorded ? knownTurns : [...knownTurns, echo],
        };
      });
    }).catch((error: unknown) => {
      /* A failure belongs to the conversation that failed. Reported on another
         one it is a sentence under a composer the reader never sent from, and
         dropping the echo there would be dropping someone else's. */
      if (!stillActive()) return;
      setEchoes((current) => current.filter((turn) => turn.id !== echo.id));
      setActionError(errorMessage(error, 'Could not send the message.'));
    }).finally(() => {
      setUnconfirmedEchoId((current) => current === echo.id ? null : current);
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

  return {
    conversations,
    turnsOf: (conversationId) => conversation?.id === conversationId
      ? transcript
      : registry.turnsOf(conversationId),
    pending: pendingConversationIds(conversation, working, sending),
    working,
    stopping,
    sending,
    hasEarlier: history.hasNextPage,
    loadingEarlier: history.isFetchingNextPage,
    historyError: history.error instanceof Error ? history.error.message : null,
    actionError,
    start: () => conversation,
    send,
    interrupt,
    loadEarlier: () => { void history.fetchNextPage().catch(() => undefined); },
  };
}

/**
 * The one conversation whose transcript is being read.
 *
 * `id` is the wave the card hangs off; `title` is that wave's title *when the
 * surface knows one*, which a cove route does not — its chat wave is hidden on
 * purpose. `kind` carries what the list row already knew, so opening a chat row
 * cannot make it read as a spec one. `state` carries the row's server state as
 * the *baseline*; the open row is the only one this route can watch live, so it
 * — and only it — also picks up the local phase (`turn_pending` while a turn is
 * in flight) and the name derived from its first message, which is why the open
 * row can show a name and a dot the closed rows cannot (§7).
 */
type SpecConversationScope = Readonly<{
  id: string;
  title?: string;
  cardId: string;
  cardTitle: string | null;
  updatedAt: number;
  kind?: ConversationKind;
  state?: ConversationState | null;
}>;

/**
 * What the panel is looking at — which is three different things, and they were
 * previously told apart by whether `scope` was null.
 *
 * That conflated "there is a card open" with two facts that have nothing to do
 * with a card: whether this route can *hold* a drawer at all, and where a new
 * conversation would go. Today cannot hold one (it has no wave and no cove), so
 * opening a row there navigates; a cove route can hold one for every row it
 * lists, so opening one must not navigate — the wave it would navigate to is
 * hidden.
 *
 * Two routes select two of the three: Today is `'elsewhere'` and both the cove
 * and the wave route are `'rows'`. **`'card'` is selected by nothing** since the
 * wave route stopped forking on its spec card (#1189 §5.3) — it is dead, and it
 * is left standing for the reason given on `ConversationListIntent`: removing it
 * also removes `store.start`, the `scope` arm of the open-request consume, and
 * the `/new` parity argument below, none of which belongs in a review fix. Read
 * every `source.kind === 'card'` branch below as unreachable today.
 */
type ConversationPanelSource =
  | Readonly<{ kind: 'elsewhere'; intent: ConversationListIntent }>
  | Readonly<{ kind: 'card'; intent: ConversationListIntent; scope: SpecConversationScope }>
  | Readonly<{
    kind: 'rows';
    /**
     * What a draft on this route belongs to — a cove id on a cove route, a wave
     * id on a wave route. Two routes, one drawer, and the reducer tells their
     * drafts apart by this value alone (`ConversationDraft`).
     */
    scopeId: string;
    rows: readonly Conversation[];
    /** See `ConversationRouteIntent`: the wave these rows may be sent to, or
     *  null when there is none and nothing may be remembered. */
    rememberOn: string | null;
    scopeOf: (conversationId: string) => SpecConversationScope | null;
    /**
     * The id the card minted under this key *will* have, derived rather than
     * observed — the two endpoints derive it from different namespaces, so this
     * cannot be one shared function of `(scopeId, key)`.
     */
    derivedCardId: (idempotencyKey: string) => string;
    create: (text: string, idempotencyKey: string) => Promise<Conversation>;
    refresh: () => Promise<readonly Conversation[]>;
  }>;

/** Which row is open, or the draft that has not become a row yet. */
type OpenTarget = Readonly<{ kind: 'row'; id: string } | { kind: 'draft' }>;

/**
 * A conversation being written: one value, not five pieces of state.
 *
 * They were five (`draftKey`, `draftText`, `sentText`, `draftError`,
 * `draftRemedy`) and every rule about them was a rule about *pairs* — "a new
 * key means nothing was sent under it yet", "the words on screen are only an
 * edit if a POST was made with different ones". Five independently settable
 * fields make each such rule a thing to remember at every branch, and the two
 * bugs this shape replaces were both one field moving without its partner.
 *
 * `scopeId` is in here for one concrete reason, and it is worth stating exactly
 * because a looser version of it was written here first and was false. The
 * panel is **not** remounted when the reader walks from one cove to another:
 * `/cove/$coveId` is one route component across a param change, so the reducer
 * survives and a draft that did not name its cove would be a draft cove B's `+`
 * picks up — posting cove A's key and words to cove B. That is what
 * `cove-conversation.test.tsx`'s `keeps a failed draft to the cove it belongs
 * to` holds down, and it is the whole of what the guard is *proved* to do.
 *
 * Cove → wave is a different walk and has no twin test, because it cannot: the
 * cove and wave routes are two sibling components under `rootRoute`, so that
 * walk unmounts the panel and takes the reducer — draft and all — with it.
 * Nothing can leak across it, and nothing survives it either (#1225 is the
 * second half of that sentence: an unsent draft, and the idempotency key that
 * makes a retry a retry, are simply lost).
 *
 * #1189 is still why the field is `scopeId` and not `coveId`: a wave route now
 * holds drafts too, and one field carrying values from two id spaces is what
 * keeps the guard total should a future route ever hold both — a generalisation
 * that costs nothing and closes the shape rather than a claim about today's
 * routing.
 */
type ConversationDraft = Readonly<{
  /** The cove or wave this draft belongs to. It is only ever visible there. */
  scopeId: string;
  /** Identifies the draft to the server for as long as it exists; minted when
   *  the drawer opens and *never* per send. */
  key: string;
  /** The words the drawer is holding, kept because `ChatComposer` clears its
   *  own field on send and returns void: without this the text is gone on any
   *  failure. Set by anything that reaches the drawer, including a local
   *  refusal that never became a request. */
  text: string | null;
  /** The words a POST was actually made with under `key`, which is a different
   *  fact from the words on screen: only this one may decide "the reader
   *  edited the text after a failure", the branch that is allowed to mint a
   *  second key. Text refused locally (too long) never travelled, so it must
   *  not count as an edit. */
  sentText: string | null;
  error: string | null;
  remedy: 'retry' | 'new-conversation' | null;
}>;

/** What a caller may change without touching the draft's identity. `key` and
 *  `sentText` are deliberately absent: they move together or not at all, which
 *  is why `rekeyDraft` and `markDraftSent` are the only doors to them. */
type DraftEdit = Partial<Pick<ConversationDraft, 'text' | 'error' | 'remedy'>>;

/**
 * The card runtime, created once at boot and injected like every other
 * instance-owned dependency. The wave route mounts visible cards into the
 * grid overlay through `host`.
 */
export type CardRuntime = Readonly<{ registry: CardRegistry; host: CardHost }>;

export type AppRouterDeps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  client: QueryClient;
  onSignOut: () => void;
  cards: CardRuntime;
}>;

export function createRouteTree({ transport, unauthorized, client, onSignOut, cards }: AppRouterDeps): AnyRoute {
  const rootRoute = createRootRoute({ component: () => <ShellRoute transport={transport} unauthorized={unauthorized} onSignOut={onSignOut} /> });

  const indexRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/',
    /**
     * INV-APP-084 — the index loader primes **only** the coves list. The
     * cove → waves fan-out stays lazy inside the page (`useQueries` in
     * `useWorkspace`); awaiting it here would let one slow cove block the
     * whole calendar behind the route commit.
     */
    loader: () => prefetchCoveList(client, transport, unauthorized),
    component: () => <TodayRoute transport={transport} unauthorized={unauthorized} />,
  });

  const coveRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/cove/$coveId',
    component: () => <CoveRoute transport={transport} unauthorized={unauthorized} />,
  });

  const waveRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/wave/$waveId',
    validateSearch: (search: Record<string, unknown>): WaveSearch => validateWaveSearch(search),
    component: () => <WaveRoute transport={transport} unauthorized={unauthorized} cardRuntime={cards} />,
  });

  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings',
    component: () => <SettingsRoute transport={transport} unauthorized={unauthorized} />,
  });

  const templateListRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/templates',
    component: () => <TemplateListRoute transport={transport} unauthorized={unauthorized} />,
  });

  const templateEditorRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings/templates/$templateId',
    component: () => <TemplateEditorRoute transport={transport} unauthorized={unauthorized} />,
  });

  return rootRoute.addChildren([
    indexRoute, coveRoute, waveRoute, settingsRoute, templateListRoute, templateEditorRoute,
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
        onSignOut={onSignOut}
      />
    </ConversationProvider>
  );
}

/** Everything needed to say "is this still the draft I was working on?": the
 *  cove or wave it belongs to and the key that is the identity of its attempt. */
type DraftId = Readonly<{ scopeId: string; key: string }>;

/**
 * What the drawer is showing and which draft is held — **one** state, moved by
 * one reducer.
 *
 * These were two `useState`s, and the split was the bug. A create is
 * asynchronous, and by the time it answers the reader may have walked to
 * another cove and started a draft there. Adopting the answer then meant two
 * independent writes — clear the held draft, point the drawer at a row — with
 * nothing tying either to the draft the request was made *for*: cove A's late
 * success deleted cove B's brand-new draft and aimed the drawer at a row that
 * is not on cove B.
 *
 * Merging them is what makes the guard expressible at all. Every move that a
 * late answer can perform carries the `DraftId` it was computed from, and the
 * reducer compares it against what is held *now*, in the same atomic update
 * that would have applied it. A move whose draft is gone changes nothing —
 * not "changes only the half that has a guard".
 */
type DrawerState = Readonly<{ open: OpenTarget | null; held: ConversationDraft | null }>;

type DrawerMove = Readonly<
  /** Point the drawer at an existing row. Touches no draft, so it needs no
   *  identity: the reader pressed a row, or a card route opened its own one. */
  | { kind: 'open-row'; id: string }
  /** Reopen the held draft (`+` on a draft that was sent and failed). */
  | { kind: 'open-draft' }
  /** A brand-new draft, which by definition replaces whatever was held. */
  | { kind: 'start-draft'; draft: ConversationDraft }
  /** A whole-object edit of the draft `from`, applied only if it is still held. */
  | { kind: 'edit-draft'; from: DraftId; next: (current: ConversationDraft) => ConversationDraft }
  /** `from` became row `id`: drop it and open the row, or do neither. */
  | { kind: 'adopt'; from: DraftId; id: string }
  /** Close the drawer, discarding `discard` if it is still the held draft. */
  | { kind: 'close'; discard: DraftId | null }
>;

const heldIs = (held: ConversationDraft | null, id: DraftId): held is ConversationDraft =>
  held !== null && held.scopeId === id.scopeId && held.key === id.key;

function moveDrawer(state: DrawerState, move: DrawerMove): DrawerState {
  switch (move.kind) {
    case 'open-row':
      return { ...state, open: { kind: 'row', id: move.id } };
    case 'open-draft':
      return { ...state, open: { kind: 'draft' } };
    case 'start-draft':
      return { open: { kind: 'draft' }, held: move.draft };
    case 'edit-draft':
      return heldIs(state.held, move.from) ? { ...state, held: move.next(state.held) } : state;
    case 'adopt':
      /* Both halves or neither. This is the whole point of the merge. */
      return heldIs(state.held, move.from) ? { open: { kind: 'row', id: move.id }, held: null } : state;
    case 'close':
      return {
        open: null,
        held: move.discard !== null && heldIs(state.held, move.discard) ? null : state.held,
      };
  }
}

/**
 * The conversation module, identical on all three routes: a list, a `+` in the
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
  options?: { showWave?: boolean },
) {
  /* One draft at a time, and it names the cove it belongs to — see
     `ConversationDraft`. It shares a reducer with the drawer's target because
     a late create has to move both or neither — see `DrawerState`. */
  const [{ open: openTarget, held }, moveDrawerTo] = useReducer(
    moveDrawer, { open: null, held: null } as DrawerState,
  );
  /* State, not a ref, unlike `store.send`'s `sendingRef`: `creating` is read by
     the render (it disables the composer and both remedy buttons) and a ref
     would not re-render. The double-submit a ref guards against cannot mint a
     second card here — both posts carry the same draft key, and the server
     collapses them onto one operation — so the guard a ref would add is one the
     idempotency key already provides. */
  const [creating, setCreating] = useState(false);
  /*
   * The conversation whose composer this route was asked to put the caret in
   * — a just-created wave's spec row (#1211 S2), and nothing else.
   *
   * It has to be held here rather than read off the registry at render time,
   * because the request is cleared in the same commit that opens the row. Read
   * once, at the composer's mount, and dropped when the drawer closes so that
   * re-opening the same row by hand is an ordinary open.
   */
  const [composerFocusFor, setComposerFocusFor] = useState<string | null>(null);

  const openRowId = openTarget?.kind === 'row' ? openTarget.id : null;
  useEffect(() => { if (openRowId === null) setComposerFocusFor(null); }, [openRowId]);
  const scope: SpecConversationScope | null = source.kind === 'card'
    ? source.scope
    : source.kind === 'rows' && openRowId !== null ? source.scopeOf(openRowId) : null;
  const routeIntent: ConversationRouteIntent = source.kind === 'rows'
    ? { kind: 'rows', rows: source.rows, rememberOn: source.rememberOn }
    : source.intent;

  const rows = source.kind === 'rows' ? source.rows : null;
  const store = useConversationStore(transport, unauthorized, scope, routeIntent);
  const registry = useConversationRegistry();
  const go = useGo();
  const open = store.conversations.find((conversation) => conversation.id === openRowId) ?? null;

  /*
   * The draft, if it belongs *here*.
   *
   * A held draft from another cove is not visible, not reopenable and not
   * sendable here; it is simply not this route's business. It is kept rather
   * than dropped so that walking back to that cove still finds it; the one
   * thing that discards it is starting a draft somewhere else, because only one
   * is held at a time.
   *
   * "Walking back" means within one route component — cove → cove. A draft
   * written on a wave is never seen by a cove for a blunter reason: the two are
   * separate route components, so the walk unmounted this hook and there is no
   * held draft left to filter (see `ConversationDraft`, and #1225). The test is
   * the same either way, and being the same is the point.
   */
  const draft = held !== null && source.kind === 'rows' && held.scopeId === source.scopeId ? held : null;

  /*
   * Every write to the draft goes through one of these three, and each is a
   * single whole-object update. `amendDraft` cannot touch the key or the words
   * a POST was made with; the two that can, move both at once.
   *
   * All three are no-ops when the draft they were computed from is no longer
   * the one held. Note what that does and does not cover, because an earlier
   * version of this comment claimed too much: walking to another cove does
   * **not** by itself discard the draft — it stays held, just invisible here,
   * so a late write from its own cove still lands on it, which is right. What
   * makes a write a no-op is the draft actually being gone: adopted, closed,
   * or replaced by a `+` pressed somewhere else. `adopt` is guarded the same
   * way and by the same identity, in the same reducer.
   */
  const withDraft = (from: DraftId, next: (current: ConversationDraft) => ConversationDraft) => {
    moveDrawerTo({ kind: 'edit-draft', from, next });
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
   * Today asked for a conversation to be opened, and this route is where it
   * lives. Two shapes, because the two sources answer "which conversation is
   * this route holding?" at different times.
   *
   * A `'card'` route knows its one card before anything is fetched, so `scope`
   * is the whole answer. A `'rows'` route has no scope at all until a row is
   * *already* open (`scope` is computed from `openRowId`), so asking `scope`
   * there is asking whether the drawer is open — which it is not, that being
   * the entire request. It has to consume the request against the rows.
   *
   * The condition is "the rows are loaded **and** contain this id", never
   * "the rows do not contain it, so clear". The list arrives a round trip
   * later than the request, and a request cleared while it was still empty is
   * lost for good — the reader lands on the wave with the drawer shut and no
   * second chance. The registry is also tab-wide, so the id may belong to
   * another wave entirely; that case is not this effect's to decide either, and
   * the route clears it from outside (`WaveRoute`, and the failed-read fallback
   * beside the rows query).
   */
  useEffect(() => {
    const requestedOpenId = registry.requestedOpenId;
    if (requestedOpenId === null) return;
    /* Captured here and not read at render time: the request is cleared in the
       same commit that opens the row, so by the time the composer mounts the
       registry no longer remembers what was asked for. */
    const focusComposer = registry.requestedOpenFocusesComposer;
    if (rows !== null) {
      if (!rows.some((row) => row.id === requestedOpenId)) return;
      moveDrawerTo({ kind: 'open-row', id: requestedOpenId });
      if (focusComposer) setComposerFocusFor(requestedOpenId);
      registry.clearOpenRequest();
      return;
    }
    if (scope === null || requestedOpenId !== scope.cardId) return;
    moveDrawerTo({ kind: 'open-row', id: scope.cardId });
    if (focusComposer) setComposerFocusFor(scope.cardId);
    registry.clearOpenRequest();
  }, [registry, rows, scope]);

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
   * The `+` opens a conversation. On a wave that is the wave's one spec card,
   * which already exists; on a cove it is a draft, because the card is minted
   * by the first message and there is nothing to open until then. On Today
   * there is neither a wave nor a cove to attach one to, so the action is not
   * offered rather than offered and refused.
   */
  const start = () => {
    if (source.kind === 'card') {
      const conversation = store.start();
      if (conversation !== null) moveDrawerTo({ kind: 'open-row', id: conversation.id });
      return;
    }
    if (source.kind !== 'rows') return;
    /*
     * A draft that was sent and failed is still open business, and `+` is the
     * only way back to it once the drawer was closed. Reopening it — same key,
     * same words, same sentence explaining what went wrong — is what makes the
     * key kept by `closeDrawer` mean anything: without this the next attempt
     * would be a fresh key, and a fresh key on top of an attempt that may have
     * committed is the second conversation this whole mechanism exists to stop.
     */
    if (draft !== null && draft.sentText !== null) {
      moveDrawerTo({ kind: 'open-draft' });
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
     * `draft` is null here whenever the held draft belongs to another cove, so
     * this branch — not the restore above — is what a `+` in a second cove
     * gets, and the key it mints is that cove's.
     */
    moveDrawerTo({
      kind: 'start-draft',
      draft: {
        scopeId: source.scopeId,
        key: mintIdempotencyKey(),
        text: null, sentText: null, error: null, remedy: null,
      },
    });
  };

  /*
   * `/new` in the composer runs `start` — the very callback the `+` runs — and
   * it is offered on exactly one of the three sources.
   *
   * `'elsewhere'` (Today): no `+` either. There is no wave and no cove to
   * attach a conversation to, so the action is not offered rather than offered
   * and refused. Strict parity, and it is what `action:` below already decides.
   *
   * `'card'` (a wave route): the `+` is offered, and `/new` is **not**. This is
   * the one place the two differ, deliberately. On a wave there is exactly one
   * spec card, so `start()` there does not create anything — it opens the row,
   * and from inside the drawer that row is the one already open, which makes
   * the command a no-op. A menu entry named `New conversation` that reopens the
   * conversation you are reading is a lie told by a control, and the honest
   * `+` gets away with it only because it is pressed from *outside* the drawer,
   * where "bring that one up" is a real outcome. Nothing is lost by omitting
   * it: the wave route has no second conversation to start.
   *
   * `'rows'` (a cove route): offered, and this is the case the whole thing
   * exists for — the `+` mints a draft, the drawer hides the panel column the
   * `+` lives on, and the reader inside a conversation would otherwise have to
   * close it first.
   */
  const startAnother = source.kind === 'rows' ? start : undefined;

  /*
   * The attempt `from` became row `row`: forget the draft and open the row.
   *
   * `from` is not decoration. This runs after an `await`, and by then the
   * reader may be in another cove with another draft in hand; the reducer
   * applies both halves only if `from` is still what is held, and otherwise
   * applies neither — see `DrawerState`. Before that guard existed, cove A's
   * late success deleted cove B's draft and pointed the drawer at a row cove B
   * does not have.
   */
  const adopt = (from: DraftId, row: Conversation) => {
    moveDrawerTo({ kind: 'adopt', from, id: row.id });
  };

  const UNCONFIRMED = 'Could not check whether the last attempt went through. Try again in a moment.';

  /*
   * Re-read the list and adopt **this draft's own row** if it is there.
   *
   * A 500, a 503 or a dropped connection does not mean nothing happened: the
   * card can exist with the message already queued behind it. What is not
   * allowed is answering that question with "the list grew". During the seconds
   * an attempt is failing, another tab or another reader can add a conversation
   * to the same cove, and adopting *that* row opens somebody else's chat as if
   * it were the words just typed — while this draft's real card, if it exists,
   * goes unclaimed.
   *
   * So the question asked is the exact one: the card id is a pure public
   * function of `(scopeId, key)` — `coveConversationCardId` on a cove and
   * `waveConversationCardId` on a wave, each golden-tested against the server's
   * own golden and each hashing its **own namespace**, which is what stops one
   * `(id, key)` pair naming one card from two endpoints. The route supplies the
   * derivation (`source.derivedCardId`); this asks it. So the row this attempt
   * would have created can be named before looking, and only that id counts.
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
  ): Promise<'landed' | 'absent' | 'unknown'> => {
    const rows = await refresh().catch(() => null);
    if (rows === null) return 'unknown';
    const cardId = derivedCardId(key);
    const landed = rows.find((row) => row.id === cardId);
    if (landed === undefined) return 'absent';
    adopt({ scopeId, key }, landed);
    return 'landed';
  };

  const sendDraft = (text: string) => {
    if (source.kind !== 'rows' || creating || draft === null) return;
    const { create, refresh, scopeId, derivedCardId } = source;
    /*
     * Two different questions, and they are asked of two different strings —
     * because the server asks them that way.
     *
     * `create_cove_conversation` refuses `text.trim().is_empty()` and then
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
    if (Array.from(text).length > COVE_CONVERSATION_TEXT_MAX) {
      /* Shown back, but never recorded as sent: no request left the browser, so
         the key is untouched and the next press is not "the text changed". */
      amendDraft(draft, {
        text,
        error: `This message is too long — the limit is ${COVE_CONVERSATION_TEXT_MAX} characters.`,
        remedy: null,
      });
      return;
    }
    const previousText = draft.sentText;
    /* The draft this send is *for*, fixed here. Everything below writes through
       it, so a send that outlives its draft — adopted, closed, or left behind by
       a cove switch — changes nothing rather than writing into whatever is held
       by then. */
    let attempt = draft;
    setCreating(true);
    amendDraft(attempt, { text, error: null, remedy: null });
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
          const landing = await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key);
          if (landing === 'landed') return;
          if (landing === 'unknown') {
            amendDraft(attempt, { error: UNCONFIRMED, remedy: 'retry' });
            return;
          }
          attempt = rekeyDraft(attempt, mintIdempotencyKey());
        }
        markDraftSent(attempt, text);
        attempt = { ...attempt, text, sentText: text };
        adopt(attempt, await create(text, attempt.key));
      } catch (error: unknown) {
        await handleCreateFailure(error, refresh, derivedCardId, scopeId, attempt);
      } finally {
        setCreating(false);
      }
    })();
  };

  async function handleCreateFailure(
    error: unknown,
    refresh: () => Promise<readonly Conversation[]>,
    derivedCardId: (idempotencyKey: string) => string,
    scopeId: string,
    attempt: ConversationDraft,
  ): Promise<void> {
    const failure = error instanceof ApiError
      ? coveConversationFailure(error.failure)
      : { kind: 'retry' as const, message: errorMessage(error, 'Could not start the conversation.') };
    const message = failure.message;
    switch (failure.kind) {
      case 'gone':
        amendDraft(attempt, { error: message, remedy: null });
        go({ name: 'today' });
        return;
      case 'exhausted':
        /* A spent key can never succeed again, so a new one is minted — and it
           takes `sentText` with it: nothing has been posted under this key, so
           the next press must not be read as "the reader changed the words".
           The words themselves are untouched, so that press is a genuinely new
           conversation carrying them. */
        rekeyDraft(attempt, mintIdempotencyKey(), { error: message, remedy: 'retry' });
        return;
      case 'stale-payload':
        amendDraft(attempt, { error: message, remedy: 'new-conversation' });
        return;
      case 'blocked':
        /* Nothing committed and the key is unspent, so both it and the words
           are kept. Whether resending them unchanged can work depends on the
           cause the sentence names — a 400 refuses the words themselves — and
           the composer is open either way. */
        amendDraft(attempt, { error: message, remedy: 'retry' });
        return;
      case 'exists': {
        /* The derived card exists, so this key can never mint again. If the
           re-read turns it up we open it; if it says there is none, only a new
           key can go anywhere and the reader decides whether to spend one. If
           the re-read could not answer, we are not entitled to offer that
           choice yet — a new key here would be a second card next to the one
           the server just told us exists. */
        amendDraft(attempt, { error: message });
        const landing = await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key);
        if (landing === 'absent') amendDraft(attempt, { remedy: 'new-conversation' });
        if (landing === 'unknown') amendDraft(attempt, { error: UNCONFIRMED, remedy: 'retry' });
        return;
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
        if (await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key) !== 'landed') {
          amendDraft(attempt, { remedy: 'retry' });
        }
        return;
    }
  }

  const sendAsNewConversation = () => {
    if (source.kind !== 'rows' || creating || draft === null || draft.text === null) return;
    const { create, refresh, scopeId, derivedCardId } = source;
    const text = draft.text;
    let attempt = draft;
    setCreating(true);
    amendDraft(attempt, { error: null, remedy: null });
    void (async () => {
      try {
        /* Pressed deliberately, but the same fence applies: a new key is only
           safe once the list has actually said the old one produced nothing. */
        const landing = await adoptIfItLanded(refresh, derivedCardId, scopeId, attempt.key);
        if (landing === 'landed') return;
        if (landing === 'unknown') {
          amendDraft(attempt, { error: UNCONFIRMED, remedy: 'new-conversation' });
          return;
        }
        attempt = rekeyDraft(attempt, mintIdempotencyKey());
        markDraftSent(attempt, text);
        attempt = { ...attempt, text, sentText: text };
        adopt(attempt, await create(text, attempt.key));
      } catch (error: unknown) {
        await handleCreateFailure(error, refresh, derivedCardId, scopeId, attempt);
      } finally {
        setCreating(false);
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
    moveDrawerTo({ kind: 'close', discard: draft !== null && draft.sentText === null ? draft : null });
  };

  /* A draft belonging to another cove is not open here even if the drawer was
     left on one: `draft` is null on any route but its own. */
  const draftOpen = openTarget?.kind === 'draft' && draft !== null;

  return {
    list: (
      <ChatList
        conversations={store.conversations}
        activeId={open?.id ?? null}
        showWave={options?.showWave ?? true}
        onOpen={(conversation) => {
          /* Only a route that cannot hold the drawer sends the reader
             somewhere else. A `'rows'` route holds one for every row it lists,
             and the wave it would navigate to is hidden. */
          if (source.kind !== 'elsewhere') {
            moveDrawerTo({ kind: 'open-row', id: conversation.id });
            return;
          }
          registry.requestOpen(conversation.id);
          go({ name: 'wave', waveId: conversation.waveId });
        }}
      />
    ),
    /* The module head's action, composed by the page — same slot the WAVES and
       CARDS modules already use, which is why this needed no new mechanism. */
    action: source.kind === 'elsewhere'
      ? undefined
      : <PanelAction label="New conversation" onClick={start}><Icon name="chat" size="sm" /></PanelAction>,
    startConversation: source.kind === 'elsewhere' ? undefined : start,
    drawer: (
      <Drawer
        open={open !== null || draftOpen}
        /* A draft has no name yet, and naming it after the words being typed
           would rename the drawer on every keystroke. */
        title={open !== null ? conversationName(open) : draftOpen ? 'Untitled' : ''}
        mobileBackLabel={source.kind === 'card' ? 'Report' : 'Conversations'}
        onClose={closeDrawer}
        footer={draftOpen ? (
          <>
            {/* No optimistic echo: the POST starts the thread *and* delivers
                this message, so by the time it answers the message is already
                persisted and the first item fetch on the new card carries it. */}
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
            {store.actionError !== null && (
              <ChatFooterNotice><ChatFooterError message={store.actionError} /></ChatFooterNotice>
            )}
            <ChatComposer
              /* Read at mount only, which is what makes it one-shot: the
                 composer mounts when the drawer opens on a row, and the flag
                 is dropped when it closes (the effect beside
                 `composerFocusFor`). #1211 S2 — a wave created from the `+`
                 lands with its spec conversation open and the caret in it,
                 because the reader's first sentence is the wave's intent. */
              focusOnMount={composerFocusFor === open.id}
              disabled={store.sending}
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
            {store.historyError !== null && <p role="alert">{store.historyError}</p>}
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
              * parked too. `cove-conversation.test.tsx` holds that down with
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
            <ChatThread
              key={open.id}
              conversation={open}
              turns={store.turnsOf(open.id)}
              pending={store.pending.has(open.id)}
            />
            {/*
              * Nothing follows the transcript.
              *
              * `Reset conversation` used to be here, one line under the last
              * reply. It is gone from the product (#1139), not moved: a cove's
              * chat wave holds as many conversations as you start, so "empty
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
  const waveMutations = useWaveMutations(transport, unauthorized);
  const deletion = useDeleteConfirm((waveId, signal) => {
    const wave = workspace.waves.find((candidate) => candidate.id === waveId);
    if (wave === undefined) throw new Error('This wave is no longer available.');
    return waveMutations.remove(wave.id, wave.coveId, signal);
  });
  /* No `+`: a conversation attaches to a wave (the kernel's sessions hang off
     a card, and cards belong to waves), and this route has no single wave in
     scope. The module still lists and still opens — it is the starting that
     needs somewhere to attach. */
  const chat = useConversationPanel(transport, unauthorized, {
    kind: 'elsewhere', intent: { kind: 'all' },
  });
  /*
   * #1253 §5.1 — the launchpad resolve, and it is a READ.
   *
   * `POST /api/today/launchpad/ensure` is deliberately not called from here,
   * and that is INV-TODAYDOC-001, not a nicety: `ensure` materializes a
   * workspace and then submits a `spec-harness-start` operation and waits on
   * it, so putting it on the page-load path would make Today fail hard
   * whenever codex is down — worse than the Today this replaces, which needed
   * nothing to render. `ensure` belongs to an explicit action; PR1 has none.
   *
   * A 404 arrives as `null` ("nothing yet" → empty state). Everything else
   * arrives as an error and is rendered as one (INV-TODAYDOC-002).
   */
  const launchpadQuery = useQuery(todayLaunchpadQueryOptions(transport, unauthorized));
  const launchpad = launchpadQuery.data;
  const launchpadWaveId = launchpad?.wave_id ?? '';
  /* The document itself comes from the ordinary wave detail — the resolve
     carries no `report_card_id` because `readWaveReport` locates the card by
     `kind === 'wave-report'` and that field would have no consumer (§5.1).

     Gated on the server's own answer, not merely on having a wave id: when
     `report_has_noninitial_content` is false there is nothing to draw, so the
     page load stays at one request. It also keeps the states below honest —
     every one of them is then about a document the reader is actually owed. */
  const launchpadHasContent = launchpad?.report_has_noninitial_content === true;
  const launchpadDetailQuery = useQuery({
    ...waveDetailQueryOptions(transport, launchpadWaveId, unauthorized),
    enabled: launchpadWaveId !== '' && launchpadHasContent,
  });
  const launchpadReport = useMemo(
    () => readWaveReport(launchpadDetailQuery.data?.cards ?? []),
    [launchpadDetailQuery.data],
  );
  /*
   * Three states, three answers — and they must not be collapsed.
   *
   * `readWaveReport(...) === null` is true in all three: while the detail is
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
          /* The ONLY remaining reason `report` can be null here: the detail
             arrived, the server says the report has been written, and this
             build could not decode the payload. Neither of the other two
             states reaches this branch any more. */
          empty={<ReportEmpty
            lead="Today's report could not be read."
            hints={[
              'The server says it has been written, so this is a decoding problem, not an empty day.',
              'The report\'s payload is probably newer than this build.',
            ]}
          />}
        />
      );
  const workspaceError = workspace.covesError
    ?? workspace.waveErrorsByCove.values().next().value ?? null;
  if (workspace.covesLoading
    || (workspace.waves.length === 0 && [...workspace.wavesLoadingByCove.values()].some(Boolean))) return null;
  return (
    <>
    {workspaceError !== null && <ErrorBox
      message={workspaceError.message}
      onRetry={() => {
        workspace.retryCoves(); workspace.retryOverlays();
        for (const cove of workspace.coves) workspace.retryWaves(cove.id);
      }}
    />}
    {workspace.overlaysError !== null && <ErrorBox message={`Wave activity is unavailable: ${workspace.overlaysError.message}`} onRetry={workspace.retryOverlays} />}
    {deletion.feedback.error !== null && <div role="alert" data-nc-error-box="">
      <span>{deletion.feedback.error}</span>
      <button type="button" data-nc-action="tertiary" onClick={deletion.feedback.clear}>Dismiss</button>
    </div>}
    <TodayPage
      waves={workspace.waves}
      coves={workspace.coves}
      // The row belongs to features/wave and Today may not import a sibling
      // domain, so the composition layer injects it — the same reason CovePage
      // takes its list as a prop. One WaveRow still, per INV-DUP-009.
      renderWaveRow={(wave, options) => (
        <WaveRow
          wave={wave}
          variant={options.variant}
          hourLabel={options.hourLabel}
          coveName={options.coveName}
          onOpen={(waveId) => go({ name: 'wave', waveId })}
          /* The panel variant only — that is the calendar's agenda, inside the
             card, where every other list already puts a delete under the status
             dot. The main column's sections stay read-only: they are the day's
             report, and a report is not a place you edit from. */
          onDelete={options.variant === 'panel' ? deletion.request : undefined}
        />
      )}
      conversationList={chat.list}
        conversationAction={chat.action}
      /* Undefined while the resolve is in flight, `null` on 404. The page
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
    />
    <ConfirmDialog
      open={deletion.open}
      title={DELETE_WAVE_COPY.title}
      description={DELETE_WAVE_COPY.description}
      confirmLabel={DELETE_WAVE_COPY.confirmLabel}
      confirmBusyLabel="Deleting…"
      confirmState={deletion.pending ? 'busy' : 'ready'}
      onConfirm={deletion.confirm}
      onCancel={deletion.cancel}
    />
    {chat.drawer}
    </>
  );
}

function CoveRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const coveId = useRouteParam('/cove/');
  const workspace = useWorkspace(transport, unauthorized);
  const coveMutations = useCoveMutations(transport, unauthorized);
  const waveMutations = useWaveMutations(transport, unauthorized);
  const waveDeletion = useDeleteConfirm((waveId, signal) => waveMutations.remove(waveId, coveId ?? '', signal));
  const go = useGo();
  /* The New wave dialog is the shell's, not this route's. Two surfaces open it
     — every cove row's `+` in the rail and this page's WAVES module head — and
     the rail is a sibling of the outlet, so a dialog owned here was reachable
     from exactly one of them. One dialog, one set of strings; this route only
     names the cove whose id the POST will send (hidden, not a form field). */
  const requestNewWave = useRequestNewWave();
  /*
   * A cove's conversations are its own, listed by the server (#1098). They are
   * plain-chat cards on the cove's hidden chat wave, so nothing here reads the
   * session registry and nothing here is written back into it: the wave they
   * belong to is not a place the reader can be sent.
   *
   * The list is intentionally not the waves' spec conversations any more. Those
   * belong to a wave and are read on that wave's page; mixing them in would put
   * rows in this panel whose drawer this route cannot open.
   */
  const conversationsQuery = useQuery({
    ...coveConversationsQueryOptions(transport, coveId ?? '', unauthorized),
    enabled: coveId !== undefined,
  });
  const coveConversations = conversationsQuery.data ?? [];
  const conversationMutations = useCoveConversationMutations(transport, coveId ?? '', unauthorized);
  const chat = useConversationPanel(transport, unauthorized, {
    kind: 'rows',
    scopeId: coveId ?? '',
    rows: coveConversations,
    /* Null, and this is the one gate on this route: every row here lives on the
       cove's hidden chat wave, and Today navigates to `conversation.waveId`
       when a row is opened. */
    rememberOn: null,
    derivedCardId: (idempotencyKey) => coveConversationCardId(coveId ?? '', idempotencyKey),
    scopeOf: (conversationId) => {
      const row = coveConversations.find((candidate) => candidate.id === conversationId);
      /* No `title`: the chat wave is hidden, so there is no wave name to pass
         down, and `showWave: false` below is what keeps the rows honest. */
      return row === undefined ? null : {
        id: row.waveId, cardId: row.id, cardTitle: row.title,
        updatedAt: row.updatedAt, kind: row.kind, state: row.state,
      };
    },
    create: conversationMutations.create,
    refresh: conversationMutations.refresh,
  }, { showWave: false });

  const cove = coveId === undefined ? undefined : coveOf(coveId, workspace.coves);
  if (cove === undefined) {
    // While the coves list is still loading we do not know whether the cove
    // exists; showing "missing" first and the real page a moment later reads
    // as a flash of a wrong answer.
    if (workspace.covesLoading) return null;
    if (workspace.covesError !== null) return <ErrorBox message={workspace.covesError.message} onRetry={workspace.retryCoves} />;
    return <PendingRoute label="Cove" owner="features/cove" missing />;
  }
  const waveError = workspace.waveErrorsByCove.get(cove.id);
  if (waveError !== null && waveError !== undefined && !workspace.wavesByCove.has(cove.id)) return <ErrorBox
    message={waveError.message}
    onRetry={() => { workspace.retryWaves(cove.id); workspace.retryOverlays(); }}
  />;
  if (workspace.wavesLoadingByCove.get(cove.id) || !workspace.wavesByCove.has(cove.id)) return null;
  const waves = workspace.wavesByCove.get(cove.id) ?? [];

  return (
    <>
      {waveError !== null && waveError !== undefined && <ErrorBox
        message={waveError.message}
        onRetry={() => { workspace.retryWaves(cove.id); workspace.retryOverlays(); }}
      />}
      {workspace.overlaysError !== null && <ErrorBox message={`Wave activity is unavailable: ${workspace.overlaysError.message}`} onRetry={workspace.retryOverlays} />}
      {/* Without this the panel would render "No conversations yet." over a
          failed read, which is a different sentence from "we could not look". */}
      {conversationsQuery.error instanceof Error && <ErrorBox
        message={`Conversations are unavailable: ${conversationsQuery.error.message}`}
        onRetry={() => { void conversationsQuery.refetch(); }}
      />}
      <CovePage
        cove={cove}
        waveCount={waves.length}
        /*
         * Always empty, and honestly so: the kernel has no cove-level document.
         * `wave-report` is a card, and cards belong to waves — there is no
         * cove-report card kind, no column, no writer. Rendering the empty
         * state here is not a placeholder for a feature being built; it is this
         * column saying what it would hold, which is what the reader needs
         * either way.
         */
        report={<ReportEmpty
          lead="This cove has no document yet."
          hints={[
            'A cove document is written by hand — notes, decisions, links you want on the way in.',
            'Each wave keeps its own report, which the agent writes as it works.',
          ]}
        />}
        onRenameCove={(name) => coveMutations.rename(cove.id, { name }).then(() => undefined)}
        onDeleteCove={(signal) => coveMutations.remove(cove.id, signal).then(() => {
          if (!signal.aborted) go({ name: 'today' });
        })}
        onRequestNewWave={() => requestNewWave(cove.id)}
        conversationList={chat.list}
        conversationAction={chat.action}
        waveList={(
          <WaveList
            waves={waves}
            coves={workspace.coves}
            variant="panel"
            emptyMessage="This cove is quiet. Start a wave."
            onOpenWave={(waveId) => go({ name: 'wave', waveId })}
            /* No pin here. The trailing column in a panel row holds exactly one
               thing at a time — the status dot, becoming the delete on hover —
               and pinning already has a permanent home in the rail, which is
               also the only place a pinned wave surfaces. */
            onDeleteWave={waveDeletion.request}
          />
        )}
      />
      <ConfirmDialog
        open={waveDeletion.open}
        title={DELETE_WAVE_COPY.title}
        description={DELETE_WAVE_COPY.description}
        confirmLabel={DELETE_WAVE_COPY.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={waveDeletion.pending ? 'busy' : 'ready'}
        onConfirm={waveDeletion.confirm}
        onCancel={waveDeletion.cancel}
      />
      <OperationFeedback feedback={waveDeletion.feedback} />
      {chat.drawer}
    </>
  );
}

/*
 * Split in two on purpose. The conversation panel's `+` needs the wave in
 * scope, and the wave is only known after the detail query resolves and three
 * early returns have run — a hook cannot live below those. So this half owns
 * the fetching and the returns, and the half below owns the hooks that need a
 * wave.
 */
function WaveRoute({ transport, unauthorized, cardRuntime }: {
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; cardRuntime: CardRuntime;
}) {
  const waveId = useRouteParam('/wave/');
  const workspace = useWorkspace(transport, unauthorized);
  const registry = useConversationRegistry();
  const detail = useQuery({
    ...waveDetailQueryOptions(transport, waveId ?? '', unauthorized),
    enabled: waveId !== undefined,
  });
  /*
   * The card Today asked for, if this wave has it and it is a conversation card
   * at all. **Both** conversation markers, not just the spec one (#1189 §5.2):
   * an assistant card is a `codex` card carrying `harness_profile: 'assistant'`,
   * and while this predicate said `isSpecHarnessPayload` alone every assistant
   * request answered `undefined` here and was cleared by the effect below —
   * before `WaveRouteBody`'s conversation list had even loaded. The Today →
   * assistant path was cut here, one level above the effect that consumes it,
   * and no change down there could have reached it.
   *
   * Reading the markers off the wave detail is also what makes the consuming
   * effect's "wait for the rows" honest: the card and the row are two views of
   * one thing, and the card arrives with the route.
   */
  const requestedCard = detail.data?.cards.find((card) => card.id === registry.requestedOpenId
    && card.kind === 'codex'
    && (isSpecHarnessPayload(card.payload) || isAssistantHarnessPayload(card.payload)));
  const detailMatchesRoute = waveId !== undefined && detail.data?.wave.id === waveId;
  useEffect(() => {
    if (registry.requestedOpenId === null || detail.isLoading || detail.isFetching) return;
    if (!detailMatchesRoute || requestedCard === undefined) registry.clearOpenRequest();
  }, [detail.isFetching, detail.isLoading, detailMatchesRoute, registry, requestedCard]);

  if (!detail.data) {
    if (detail.isLoading || detail.isFetching) return null;
    if (detail.error instanceof Error) return <ErrorBox message={detail.error.message} onRetry={() => { void detail.refetch(); }} />;
    return <PendingRoute label="Wave" owner="features/wave" missing />;
  }
  // `detail.data` can still be the previously-viewed wave while this one
  // fetches; rendering it under this URL would show the wrong wave.
  if (waveId !== undefined && detail.data.wave.id !== waveId) return null;

  const detailActivity = waveActivityFrom(detail.data.wave.id, detail.data.overlays);
  const wave = toWave(detail.data.wave, detailActivity);

  return (
    <WaveRouteBody
      key={wave.id}
      transport={transport}
      unauthorized={unauthorized}
      wave={wave}
      cove={coveOf(wave.coveId, workspace.coves)}
      cards={detail.data.cards}
      cardRuntime={cardRuntime}
    />
  );
}

function WaveRouteBody({ transport, unauthorized, wave, cove, cards, cardRuntime }: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  wave: Wave;
  cove: Cove | undefined;
  cards: WaveDetailWire['cards'];
  cardRuntime: CardRuntime;
}) {
  const waveMutations = useWaveMutations(transport, unauthorized);
  const conversationMutations = useWaveConversationMutations(transport, wave.id, unauthorized);
  const openMobileSection = useOpenMobileSection();
  const go = useGo();
  const goSameWave = useGoSameWave();
  const { openPanel, closePanel } = useWavePanelNavigation();
  const requestedCardId = useRouteCardId();
  /*
   * The URL is read, validated and turned into props **here**, in `app/**`:
   * `features-no-app` is an error-level dependency-cruiser rule, so `WavePage`
   * cannot reach the router at all and stays a pure renderer (#1191 §2.4).
   *
   * A live `?card=` wins over `?panel=`. The two describe one surface and
   * `buildWaveSearch` already refuses to emit both, so this only decides what a
   * hand-edited URL means — and it means the card, the older deep-linkable one.
   */
  const routePanel = useRoutePanel();
  /* The one viewport question the application asks (§3.2); here it decides
     whether `?panel=` describes anything at all — see the effect below. */
  const compactViewport = useCompactViewport();
  /*
   * `?from=` is a property of *this* visit to the report, so every move that
   * stays on this wave has to hand it back explicitly — `go` clears whatever it
   * is not given (#1191 §1.3). Crossing to another wave drops it, because the
   * return path belonged to the wave being left.
   */
  const routeFrom = useRouteFrom() ?? undefined;
  const cardRegistry = cardRuntime.registry;
  // `showWave: false` — on a wave's own page the wave's name is the page title,
  // so repeating it on every row is one column spent saying nothing.
  // The same predicate the spec entry resolves by (`INV-CARD-182`), imported
  // rather than copied: hiding the card from CARDS and giving it a drawer are
  // one decision, and two hand-written copies would drift apart silently.
  const specCard = cards.find((card) => card.kind === 'codex' && isSpecHarnessPayload(card.payload));
  const registry = useConversationRegistry();
  /*
   * ── Redeeming "open the spec conversation of the wave I just created" ────
   *
   * The intent rides on the history entry the create navigated to
   * (`useSpecOpenIntent`), so `armed` is already "this wave, this visit": no
   * other route body can see it, and there is no global slot for one of them
   * to clear out from under another. What is left here is the half only this
   * component knows — which card the intent names. `POST /api/waves` answers
   * with a `Wave`, and the spec card's id exists only once the detail has
   * landed, which is here.
   *
   * `disarm()` before the open, unconditionally: a wave with no spec card has
   * nothing to open, and an intent left armed on this entry would fire on the
   * next visit to it (the Back button reaches one).
   *
   * `focusComposer` is what makes the landing complete: the wave is unnamed
   * and empty, and the reader's first sentence *is* the intent, so the caret
   * has to be where they can type it.
   */
  const specOpenIntent = useSpecOpenIntent(wave.id);
  useEffect(() => {
    if (!specOpenIntent.armed) return;
    specOpenIntent.disarm();
    if (specCard === undefined) return;
    registry.requestOpen(specCard.id, { focusComposer: true });
  }, [registry, specCard, specOpenIntent]);
  /*
   * The wave's assistant conversations (#1189). Its own endpoint, its own list;
   * the spec card is deliberately not in it — the server's list predicate is
   * `role == Assistant` — so the row for it is injected below.
   */
  const conversationsQuery = useQuery(waveConversationsQueryOptions(transport, wave.id, unauthorized));
  const assistantRows = useMemo(() => conversationsQuery.data ?? [], [conversationsQuery.data]);
  const waveTitle = waveDisplayTitle(wave.title);
  /*
   * The wave's opening conversation, derived from the spec card rather than
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
  const specRow = useMemo<Conversation | null>(() => specCard === undefined ? null : {
    id: specCard.id,
    waveId: wave.id,
    waveTitle,
    title: specCard.title,
    kind: 'shared-spec',
    state: null,
    updatedAt: specCard.updated_at,
  }, [specCard, wave.id, waveTitle]);
  /* Every row carries the wave's title, so a row that reaches Today can say
     where it is. On this page `showWave: false` hides it again.
     *
     * Unconditionally, and *before* the spec row is considered: whether this
     * wave happens to have a spec card has nothing to do with whether its
     * assistant rows know where they live, and while the two were one
     * expression the `specRow === null` arm returned the rows untouched. A
     * reader who had only ever visited waves without a spec card then saw a
     * Today list of rows reading `Assistant` with no wave named on any of
     * them — the waves that most need the `+` (§5.3) losing the label first. */
  const placedRows = useMemo(
    () => assistantRows.map((row) => ({ ...row, waveTitle })),
    [assistantRows, waveTitle],
  );
  const rows = useMemo<readonly Conversation[]>(
    () => specRow === null ? placedRows : [specRow, ...placedRows],
    [placedRows, specRow],
  );
  /*
   * `'rows'`, unconditionally — no longer `'card'` when a spec card exists and
   * `'elsewhere'` when it does not (§5.3).
   *
   * The branch that is gone took the `+` away from exactly the waves that need
   * it most: a wave with no spec card had no conversation at all and no way to
   * start one. The list being empty is a state this panel already renders, and
   * an empty list with a `+` over it is the whole feature.
   */
  const chat = useConversationPanel(
    transport,
    unauthorized,
    {
      kind: 'rows',
      scopeId: wave.id,
      rows,
      /* Unlike a cove's, these rows are on a wave the reader can be sent to —
         this very route — so Today may hold and open them. The store checks
         each row's `waveId` against this, so a row from anywhere else is not
         remembered whatever put it in the list. */
      rememberOn: wave.id,
      derivedCardId: (idempotencyKey) => waveConversationCardId(wave.id, idempotencyKey),
      scopeOf: (conversationId) => {
        /* The spec row first: it is not in `assistantRows`, and without this
           arm the one conversation a wave has always had would stop opening. */
        if (specCard !== undefined && conversationId === specCard.id) {
          return {
            id: wave.id, title: waveTitle, cardId: specCard.id,
            cardTitle: specCard.title, updatedAt: specCard.updated_at,
            kind: 'shared-spec', state: null,
          };
        }
        const row = assistantRows.find((candidate) => candidate.id === conversationId);
        /*
         * `id: row.waveId` — the row's own wave, never `wave.id`.
         *
         * This is the line the whole `rememberOn` defence rests on. `scope.id`
         * becomes `conversation.waveId` in the store, which is what
         * `conversation.waveId !== rememberOn` compares against the wave this
         * route claimed. Written `id: wave.id` it would be a tautology — every
         * open row would pass, whatever wave it really belongs to — and no
         * existing test would notice: the fixtures list rows of this wave, so
         * the two values are equal in every green case. The comparison is only
         * a comparison because this side is the row's.
         */
        return row === undefined ? null : {
          id: row.waveId, title: waveTitle, cardId: row.id, cardTitle: row.title,
          updatedAt: row.updatedAt, kind: row.kind, state: row.state,
        };
      },
      create: conversationMutations.create,
      refresh: conversationMutations.refresh,
    },
    { showWave: false },
  );
  /*
   * The fallback clear, for the one case the effects on both sides leave open.
   *
   * `WaveRoute` clears a request whose card this wave does not have, and the
   * panel consumes one whose row it does. What neither covers is a request for
   * a card this wave *does* have while the list that would open it could not be
   * read: the panel is right to keep waiting, and the wait would never end. The
   * reader then walks into this wave some other day and the drawer springs open
   * for a conversation they asked about once.
   *
   * So: the read is over, it failed, and the id is not among whatever rows did
   * arrive. Not "the read failed", which would also throw away a perfectly
   * openable spec row.
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
   * Deliberately keyed `['wave-report', waveId]`, which is the key
   * `core/events/invalidation-plan` has always planned for every `task.*`
   * event and for `wave.report_edited` — naming it anything else would have
   * meant a second, hand-rolled refresh path for a panel the event plan
   * already knew how to keep live.
   */
  /* Read before the query, not inside the join below it: the poll's own
     interval is a question about the rows this report can produce, so the
     declarations have to be in hand when the query options are built. */
  const report = useMemo(() => readWaveReport(cards), [cards]);
  const reportBlocks = report?.blocks ?? null;
  const verdictsQuery = useQuery(
    waveTaskVerdictsQueryOptions(transport, wave.id, unauthorized, reportBlocks),
  );
  const verdicts = verdictsQuery.data;
  const { outline, tasks: joinedTasks } = useMemo(() => ({
    outline: deriveReportOutline(reportBlocks),
    /* The declarations arrive with the wave detail and the verdicts land a
       round-trip later; passing `undefined` through as "none yet" renders the
       same statusless list this panel shipped with rather than a hole. */
    tasks: deriveReportTasks(reportBlocks, verdicts),
  }), [reportBlocks, verdicts]);
  /*
   * INV-CARD-226 — the CARDS module lists cards that have a surface. `spec` and
   * `wave-report` resolve to headless adapters and are dropped; anything no
   * adapter claimed stays, because an unlisted card is worse than an
   * unrecognised one. Both branches carry `originalIndex`, bound before the
   * filter, and the merge re-sorts on it so the panel keeps the wire order the
   * kernel sent — a post-filter index here would address the wrong card the
   * moment remove/action callbacks land (S2).
   */
  const panelCards = useMemo(() => {
    const { visible, unknown } = partitionWaveCards(cardRegistry, cards);
    return [...visible, ...unknown]
      .sort((left, right) => left.originalIndex - right.originalIndex)
      .map((slot) => slot.wire);
  }, [cardRegistry, cards]);
  const gridItems: readonly BoardHostItem[] = useMemo(() => {
    const { visible } = partitionWaveCards(cardRegistry, cards);
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
   * it used to have. Filtering here rather than teaching `WavePage` about the
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
    goSameWave(wave.id, { card: undefined }, { replace: true });
  }, [goSameWave, knownCard, requestedCardId, wave.id]);
  /*
   * `?panel=` is a *compact* concept, and above the breakpoint it does not just
   * sit there unused — it takes the desktop panel down with it.
   *
   * `WavePage` derives `mobilePanelOpen` from this prop alone and puts `inert` +
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
    goSameWave(wave.id, { panel: undefined }, { replace: true });
  }, [compactViewport, goSameWave, routePanel, wave.id]);
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
    (cardId, signal) => waveMutations.removeCard(wave.id, cardId, signal),
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
   *     `POST /api/waves/:id/cards` with the kind's own `buildPayload`, and the
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
   * then yank them back to a wave they deliberately left. The shape is the one
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
       `useTheme()`: subscribing here would re-render the wave subtree on every
       theme toggle and remount any live terminal under it (#177). */
    const theme = readHostThemeRgb();
    if (entry.type === 'terminal') {
      return waveMutations.createTerminal(wave.id, { theme, ...(title === undefined ? {} : { title }) });
    }
    if (entry.type === 'codex') {
      const cwd = given('cwd');
      return waveMutations.createCodex(wave.id, {
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
    return waveMutations.createCard(wave.id, {
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
       a live `goSameWave` that steers a reader who has since gone elsewhere.
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
          goSameWave(wave.id, { card: card.id });
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
  const backlinksQuery = useQuery(waveBacklinksQueryOptions(transport, wave.id, unauthorized));
  const backlinks = backlinksQuery.data;

  /*
   * A `neige://wave/…` citation. Same wave — the common case, since a report
   * mostly cites its own sections — reveals immediately so activating an
   * unchanged hash still flashes the destination, then records that destination
   * in the URL. The route body is keyed by wave id, so the hash update preserves
   * the document. Another wave is a real navigation carrying the same hash.
   */
  const arrivalAnchorId = useRouteHash();
  const openReportLink = (target: ReportLinkTarget) => {
    if (target.waveId === wave.id) {
      if (target.blockId !== null) revealReportAnchor(target.blockId);
      // Same wave: landing on a block is a move *within* the report, so the
      // return surface survives and the panel closes (the document is now what
      // the reader is looking at).
      go({ name: 'wave', waveId: target.waveId, blockId: target.blockId ?? undefined, from: routeFrom });
      return;
    }
    // Another wave: a real navigation. Nothing carries over.
    go({ name: 'wave', waveId: target.waveId, blockId: target.blockId ?? undefined });
  };

  const openReportAnchor = (blockId: string) => {
    revealReportAnchor(blockId);
    go({ name: 'wave', waveId: wave.id, blockId, from: routeFrom });
  };

  return (
    <>
    <WaveStage>
    <WavePage
      wave={wave}
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
      onOpenCard={(cardId) => { go({ name: 'wave', waveId: wave.id, cardId, from: routeFrom }); }}
      onDeleteCard={cardDeletion.request}
      board={
        <CardGridOverlay
          open={knownCard}
          items={gridItems}
          host={cardRuntime.host}
          activeCardId={requestedCardId}
          onRemoveCard={cardDeletion.request}
          onClose={knownCard
            ? () => { go({ name: 'wave', waveId: wave.id, from: routeFrom }, { replace: true }); }
            : undefined}
        />
      }
      onCloseBoard={knownCard
        ? () => { go({ name: 'wave', waveId: wave.id, from: routeFrom }, { replace: true }); }
        : undefined}
      /* Gated on the viewport, not only on the card: the effect above cannot
         have run yet on a desktop cold start, and one render with the panel
         "open" is one render with the desktop panel `inert`. */
      panel={renderedMobilePanel(routePanel, { compact: compactViewport, cardOpen: requestedCardId !== null })}
      onOpenPanel={(kind) => { openPanel(wave.id, kind); }}
      onClosePanel={() => { closePanel(wave.id); }}
      /* `?from=` is the whole memory of how the reader got here; absent means
         Pages, which is the default this route shipped with (§1.2). The cove to
         return to is the wave's own, not a stored restore id. */
      mobileBackLabel={routeFrom === 'cove' ? 'Waves' : 'Pages'}
      onMobileBack={() => {
        if (routeFrom === 'cove') openMobileSection('coves', wave.coveId);
        else openMobileSection('pages');
      }}
      report={<ReportDocument
        report={report}
        rail={<ReportOutline items={outline} />}
        backlinkCounts={backlinks === undefined ? undefined : backlinkCountsByBlock(backlinks.backlinks)}
        onOpenLink={openReportLink}
        arrivalAnchorId={arrivalAnchorId}
        /*
          #1211 S2 — "Nothing written here yet." described a missing artefact,
          and it read as an omission the reader had made. It is not one: a wave
          now starts with no name and no words in it *by design*, and the true
          state of this page on arrival is "this wave has not taken shape yet
          — say the first thing". The lead says that, and the first hint names
          the one action that changes it, which is the conversation already
          open beside it.
        */
        empty={<ReportEmpty
          lead="This wave has not taken shape yet."
          hints={[
            'Say what you want in the conversation — the agent works it out with you and writes it up here.',
            'It stays with the wave, so it is here the next time you open it.',
          ]}
        />}
      />}
      backlinks={backlinks !== undefined && backlinks.backlinks.length > 0
        ? (
          <ReportBacklinks
            waveId={wave.id}
            backlinks={backlinks}
            onOpen={(waveId, blockId) => {
              // Same wave keeps the return surface; a backlink into another wave
              // is a departure and carries nothing.
              go({ name: 'wave', waveId, blockId, from: waveId === wave.id ? routeFrom : undefined });
            }}
          />
        )
        : undefined}
      conversationList={chat.list}
      conversationAction={chat.action}
      onStartConversation={chat.startConversation}
      onRenameWave={(title) => waveMutations.patch(wave.id, wave.coveId, { title }).then(() => undefined)}
      onDeleteWave={(signal) => waveMutations.remove(wave.id, wave.coveId, signal).then(() => {
        if (signal.aborted) return;
        if (cove !== undefined) go({ name: 'cove', coveId: cove.id });
        else go({ name: 'today' });
      })}
    />
    </WaveStage>
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

function SettingsRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const go = useGo();
  const theme = useTheme();
  const save = useSettingsMutation(transport, unauthorized);
  const settings = useQuery(settingsQueryOptions(transport, unauthorized));
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  return (
    <SettingsPage
      settings={settings.data?.settings}
      onOpenTemplates={() => go({ name: 'settings-templates' })}
      loadError={settings.error instanceof Error ? settings.error.message : null}
      onRetryLoad={() => { void settings.refetch(); }}
      saving={saving}
      saveError={saveError}
      savedAt={savedAt}
      onOpenToday={() => go({ name: 'today' })}
      // `app/theme` and `features/settings` each own their copy of the mode
      // union — features may not import app. The adaptation is here, and the
      // two unions are only kept in step by this line.
      themeMode={theme.mode satisfies SettingsThemeMode}
      onThemeModeChange={(mode) => theme.setMode(mode)}
      onSave={(patch) => {
        setSaving(true);
        setSaveError(null);
        return save(patch)
          .then(() => { setSavedAt(Date.now()); })
          .catch((error: unknown) => {
            setSaveError(error instanceof Error ? error.message : 'Save failed.');
          })
          .finally(() => { setSaving(false); });
      }}
    />
  );
}

/**
 * Settings › Templates (#1230). Two routes rather than page-local state, so
 * Back leaves the editor instead of leaving Settings, and one template's
 * editor is a URL.
 *
 * Both read the **template list** — the same read the New wave picker uses.
 * There is no per-template endpoint: the list already carries `id` / `title` /
 * `tasks[{key, goal}]`, and a second read would be a second authority for the
 * same facts plus an N+1 whose failure modes have to be reasoned about apart
 * from the list's.
 */
function TemplateListRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const go = useGo();
  const templates = useWaveTemplates(transport, unauthorized);
  return (
    <TemplateListPage
      templates={templates.loaded ? templates.templates : undefined}
      loadError={templates.error}
      onRetryLoad={templates.refetch}
      onOpenSettings={() => go({ name: 'settings' })}
      onEdit={(templateId) => go({ name: 'settings-template', templateId })}
    />
  );
}

function TemplateEditorRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const templateId = useRouteParam('/settings/templates/');
  /*
   * Keyed on the id: `saving` / `saveError` / `savedAt` are per-template facts,
   * and this route component is reused across ids (same route, same instance).
   * Without the key, one template's "Saved." and error banner render over the
   * next template's editor, and an in-flight save suppresses the re-seed so
   * template A's rows show under template B's title.
   */
  return (
    <TemplateEditorSave
      key={templateId ?? ''}
      templateId={templateId}
      transport={transport}
      unauthorized={unauthorized}
    />
  );
}

function TemplateEditorSave({ templateId, transport, unauthorized }: {
  templateId: string | undefined;
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
}) {
  const go = useGo();
  const templates = useWaveTemplates(transport, unauthorized);
  const saveTemplate = useWaveTemplateMutation(transport, unauthorized);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const template = templates.loaded
    ? templates.templates.find((entry) => entry.id === templateId)
    : undefined;
  return (
    <TemplateEditorPage
      template={template}
      /* An id that names no template is a load failure for this route, not an
         empty editor: blank fields for a template that does not exist would
         invite a save against nothing. */
      loadError={templates.loaded && template === undefined
        ? `No template named ${templateId ?? ''}.`
        : templates.error}
      onRetryLoad={templates.refetch}
      saving={saving}
      saveError={saveError}
      savedAt={savedAt}
      onOpenTemplates={() => go({ name: 'settings-templates' })}
      onSave={(save) => {
        setSaving(true);
        setSaveError(null);
        return saveTemplate(save)
          .then(() => { setSavedAt(Date.now()); })
          .catch((error: unknown) => {
            setSaveError(error instanceof Error ? error.message : 'Save failed.');
          })
          .finally(() => { setSaving(false); });
      }}
    />
  );
}
