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
import type { BoardHostItem, CardAddMenuEntry, CardHost, CardRegistry } from '../../systems/cards/public.js';
import { cardAddMenuEntries, isSpecHarnessPayload, partitionWaveCards } from '../../systems/cards/public.js';
import { mintIdempotencyKey } from './idempotency-key.ts';
import { CovePage } from '../../features/cove/page/public.tsx';
import { SettingsPage, type ThemeMode as SettingsThemeMode } from '../../features/settings/public.tsx';
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
  buildTranscript, conversationName, conversationNameFrom, coveConversationCardId,
  coveConversationFailure, COVE_CONVERSATION_TEXT_MAX, harnessItemToTurn, mergeTranscript, reconcileUserEchoes,
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
  useSettingsMutation, useSpecMutations, useWaveMutations, useWorkspace,
  waveBacklinksQueryOptions, waveDetailQueryOptions, waveTaskVerdictsQueryOptions,
} from '../providers/queries.ts';
import { AppShell, useOpenMobileSection, useRequestNewWave } from '../shell/public.tsx';
import { useTheme } from '../theme/public.tsx';
import { ConversationProvider, useConversationRegistry } from '../conversations/public.tsx';
import {
  renderedMobilePanel,
  useGo, useGoSameWave, useRouteCardId, useRouteFrom, useRouteHash, useRoutePanel, useRouteParam,
  useWavePanelNavigation, validateWaveSearch, type WaveSearch,
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
 * opened — because no endpoint lists a wave's conversations. `'rows'` is the
 * opposite: the server sends the list, so the registry is not consulted at all
 * and nothing on the route is written back into it.
 */
type ConversationListIntent = Readonly<
  { kind: 'all' } | { kind: 'waves'; waveIds: readonly string[] }
>;

type ConversationRouteIntent =
  | ConversationListIntent
  | Readonly<{ kind: 'rows'; rows: readonly Conversation[] }>;

export function pendingConversationIds(
  conversation: Conversation | null, working: boolean, sending: boolean,
): ReadonlySet<string> {
  return (working || sending) && conversation !== null ? new Set([conversation.id]) : new Set();
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message !== '' ? error.message : fallback;
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
  const history = useInfiniteQuery({
    ...harnessItemsQueryOptions(transport, cardId, unauthorized), enabled: scope !== null,
  });
  const run = useQuery({ ...specRunQueryOptions(transport, cardId, unauthorized), enabled: scope !== null });
  const mutations = useSpecMutations(transport, cardId, unauthorized);
  const [echoes, setEchoes] = useState<readonly ConversationTurn[]>([]);
  const [sending, setSending] = useState(false);
  const [interruptPending, setInterruptPending] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const sendingRef = useRef(false);
  const seq = useRef(0);
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
    setActionError(null);
    sendingRef.current = false;
    setSending(false);
    setInterruptPending(false);
  }, [cardId]);

  const turns = useMemo(
    () => [...serverTurns, ...echoes].sort((left, right) => left.atMs - right.atMs),
    [echoes, serverTurns],
  );
  /* An echo is a message you already sent, so it belongs after everything the
     server has confirmed. Keep `buildTranscript`'s positional pairing intact:
     a completed action retains the started row's place even when its end time
     is later than an interleaved message. A completed tail thought stops being
     the tail as soon as the user speaks again. */
  const transcript = useMemo(() => mergeTranscript(serverEntries, echoes), [echoes, serverEntries]);
  const phase = run.data?.phase ?? null;
  const working = phase === 'issuing_turn' || phase === 'turn_running';
  const stopping = phase === 'issuing_interrupt' || interruptPending;
  const conversation = useMemo<Conversation | null>(() => waveId === undefined ? null : {
    id: cardId, waveId,
    /* Absent, not `''`: a cove conversation's wave is hidden and has no title
       to show. `ChatList` renders the difference; `''` would render a blank. */
    ...(waveTitle === undefined ? {} : { waveTitle }),
    title: cardTitle ?? conversationNameFrom(turns.find((turn) => turn.author === 'you')?.text ?? ''),
    kind: scopeKind,
    /* A chat card's state is the server's to report — `run_status_for` writes
       `turn_pending`, never `running`, for a headless harness, and everything
       outside the four live states arrives as `null`. The local phase still
       wins while a turn is in flight, because the list would otherwise sit on
       the state the last fetch happened to catch. */
    state: scopeKind === 'shared-chat'
      ? (working ? 'turn_pending' : scopeState)
      : (working ? 'running' : 'idle'),
    updatedAt: turns.at(-1)?.atMs ?? scopeUpdatedAt ?? 0, turns: turns.length,
  }, [cardId, cardTitle, scopeKind, scopeState, scopeUpdatedAt, turns, waveId, waveTitle, working]);
  useEffect(() => {
    if (conversation === null) return;
    /*
     * A server-listed conversation is never remembered.
     *
     * The registry exists so a conversation stays visible on routes that cannot
     * fetch it — which is exactly what a `'rows'` route can do for itself. What
     * remembering would add is a leak: these rows live on a cove's hidden chat
     * wave, and Today lists everything the registry holds and navigates to
     * `conversation.waveId` when a row is opened, which would walk the user
     * into the hidden wave. This one gate is the whole defence; Today has no
     * second filter, so removing it is immediately visible in the tests.
     */
    if (serverRows !== null) return;
    // Remember the full transcript so reopening the conversation preserves its
    // activity lines and looks identical to the route the user just left.
    registry.remember(conversation, transcript);
  }, [conversation, registry, serverRows, transcript]);

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
    const echo = { id: `echo-${seq.current}`, author: 'you' as const, text, atMs: Date.now() };
    setEchoes((current) => [...current, echo]);
    void mutations.send(text).catch((error: unknown) => {
      setEchoes((current) => current.filter((turn) => turn.id !== echo.id));
      setActionError(errorMessage(error, 'Could not send the message.'));
    }).finally(() => {
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
 */
type ConversationPanelSource =
  | Readonly<{ kind: 'elsewhere'; intent: ConversationListIntent }>
  | Readonly<{ kind: 'card'; intent: ConversationListIntent; scope: SpecConversationScope }>
  | Readonly<{
    kind: 'rows';
    coveId: string;
    rows: readonly Conversation[];
    scopeOf: (conversationId: string) => SpecConversationScope | null;
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
 * `coveId` is in here for the same reason: the panel is not remounted when the
 * reader walks from one cove to another (proved by
 * `keeps a failed draft to the cove it belongs to`), so a draft that does not
 * name its cove is a draft any cove's `+` can pick up — and posting cove A's
 * key and words to cove B mints a conversation in the wrong place.
 */
type ConversationDraft = Readonly<{
  /** The cove this draft belongs to. It is only ever visible there. */
  coveId: string;
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

  return rootRoute.addChildren([indexRoute, coveRoute, waveRoute, settingsRoute]);
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
 *  cove it belongs to and the key that is the identity of its attempt. */
type DraftId = Readonly<{ coveId: string; key: string }>;

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
  held !== null && held.coveId === id.coveId && held.key === id.key;

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

  const openRowId = openTarget?.kind === 'row' ? openTarget.id : null;
  const scope: SpecConversationScope | null = source.kind === 'card'
    ? source.scope
    : source.kind === 'rows' && openRowId !== null ? source.scopeOf(openRowId) : null;
  const routeIntent: ConversationRouteIntent = source.kind === 'rows'
    ? { kind: 'rows', rows: source.rows }
    : source.intent;

  const store = useConversationStore(transport, unauthorized, scope, routeIntent);
  const registry = useConversationRegistry();
  const go = useGo();
  const open = store.conversations.find((conversation) => conversation.id === openRowId) ?? null;

  /*
   * The draft, if it belongs *here*.
   *
   * A held draft from another cove is not visible, not reopenable and not
   * sendable on this one — it is simply not this route's business. It is kept
   * rather than dropped so that walking back to its own cove still finds it;
   * the one thing that discards it is starting a draft somewhere else, because
   * only one is held at a time.
   */
  const draft = held !== null && source.kind === 'rows' && held.coveId === source.coveId ? held : null;

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

  useEffect(() => {
    if (scope === null || registry.requestedOpenId !== scope.cardId) return;
    moveDrawerTo({ kind: 'open-row', id: scope.cardId });
    registry.clearOpenRequest();
  }, [registry, scope]);

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
        coveId: source.coveId,
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
   * function of `(coveId, key)` (`coveConversationCardId`, golden-tested against
   * the server's own golden), so the row this attempt would have created can be
   * named before looking, and only that id counts.
   *
   * Three answers, not two. `'unknown'` is the re-read *itself* failing, which
   * is the likeliest thing to happen while the network is the reason we are
   * here at all — and it is emphatically not `'absent'`. A caller that treats
   * "I could not look" as "there is nothing there" mints a new key over an
   * attempt that may well have committed, which is the second conversation.
   */
  const adoptIfItLanded = async (
    refresh: () => Promise<readonly Conversation[]>, coveId: string, key: string,
  ): Promise<'landed' | 'absent' | 'unknown'> => {
    const rows = await refresh().catch(() => null);
    if (rows === null) return 'unknown';
    const cardId = coveConversationCardId(coveId, key);
    const landed = rows.find((row) => row.id === cardId);
    if (landed === undefined) return 'absent';
    adopt({ coveId, key }, landed);
    return 'landed';
  };

  const sendDraft = (text: string) => {
    if (source.kind !== 'rows' || creating || draft === null) return;
    const { create, refresh, coveId } = source;
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
          const landing = await adoptIfItLanded(refresh, coveId, attempt.key);
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
        await handleCreateFailure(error, refresh, coveId, attempt);
      } finally {
        setCreating(false);
      }
    })();
  };

  async function handleCreateFailure(
    error: unknown,
    refresh: () => Promise<readonly Conversation[]>,
    coveId: string,
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
        const landing = await adoptIfItLanded(refresh, coveId, attempt.key);
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
        if (await adoptIfItLanded(refresh, coveId, attempt.key) !== 'landed') {
          amendDraft(attempt, { remedy: 'retry' });
        }
        return;
    }
  }

  const sendAsNewConversation = () => {
    if (source.kind !== 'rows' || creating || draft === null || draft.text === null) return;
    const { create, refresh, coveId } = source;
    const text = draft.text;
    let attempt = draft;
    setCreating(true);
    amendDraft(attempt, { error: null, remedy: null });
    void (async () => {
      try {
        /* Pressed deliberately, but the same fence applies: a new key is only
           safe once the list has actually said the old one produced nothing. */
        const landing = await adoptIfItLanded(refresh, coveId, attempt.key);
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
        await handleCreateFailure(error, refresh, coveId, attempt);
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
    coveId: coveId ?? '',
    rows: coveConversations,
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
  const requestedCard = detail.data?.cards.find((card) => card.id === registry.requestedOpenId
    && card.kind === 'codex' && isSpecHarnessPayload(card.payload));
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
  const chat = useConversationPanel(
    transport,
    unauthorized,
    specCard === undefined
      ? { kind: 'elsewhere', intent: { kind: 'waves', waveIds: [wave.id] } }
      : {
        kind: 'card',
        intent: { kind: 'waves', waveIds: [wave.id] },
        scope: {
          id: wave.id, title: waveDisplayTitle(wave.title), cardId: specCard.id,
          cardTitle: specCard.title, updatedAt: specCard.updated_at,
        },
      },
    { showWave: false },
  );
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
    setCreatingCard(true);
    void cardCreateFeedback
      .run(
        createCardOfKind(entry, values).then((card) => {
          setCardDraft(null);
          goSameWave(wave.id, { card: card.id });
        }),
        `Could not create the ${entry.label} card.`,
      )
      .finally(() => { setCreatingCard(false); });
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
        empty={<ReportEmpty
          lead="Nothing written here yet."
          hints={[
            'The agent writes this report as it works — start a conversation and it fills in.',
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
