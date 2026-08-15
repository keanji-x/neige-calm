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
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { CovePage } from '../../features/cove/page/public.tsx';
import { NewWaveForm, type NewWaveDraft } from '../../features/cove/new-wave/public.tsx';
import { SettingsPage, type ThemeMode as SettingsThemeMode } from '../../features/settings/public.tsx';
import { TodayPage } from '../../features/today/public.tsx';
import { WaveList } from '../../features/wave/list/public.tsx';
import { WaveRow } from '../../features/wave/row/public.tsx';
import { WavePage } from '../../features/wave/page/public.tsx';
import { ChatList } from '../../features/chat/list/public.tsx';
import { ChatComposer, ChatThread } from '../../features/chat/thread/public.tsx';
import { ReportBacklinks } from '../../features/report/backlinks/public.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ReportEmpty } from '../../features/report/empty/public.tsx';
import { ReportOutline } from '../../features/report/outline/public.tsx';
import { revealReportAnchor } from '../../features/report/anchor/public.ts';
import {
  backlinkCountsByBlock, deriveReportOutline, readWaveReport, type ReportLinkTarget,
} from '../../../../core/domain/report.ts';
import {
  buildTranscript, conversationName, conversationNameFrom, harnessItemToTurn, mergeTranscript,
  reconcileUserEchoes,
  type Conversation, type ConversationTurn, type TranscriptEntry,
} from '../../../../core/domain/conversation.ts';
import { ConfirmDialog, Dialog } from '../../ui/dialog/public.tsx';
import { DELETE_WAVE_COPY } from '../../ui/confirm-dialog/copy.ts';
import { OperationFeedback, useDeleteConfirm } from '../../ui/operation-feedback/public.tsx';
import { Drawer, DrawerAction } from '../../ui/drawer/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { PanelAction } from '../../ui/panel-card/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  ApiError, harnessItemsQueryOptions, prefetchCoveList, settingsQueryOptions, specRunQueryOptions,
  useCoveMutations, useSettingsMutation, useSpecMutations, useWaveMutations, useWorkspace,
  waveBacklinksQueryOptions, waveDetailQueryOptions,
} from '../providers/queries.ts';
import { AppShell } from '../shell/public.tsx';
import { useTheme } from '../theme/public.tsx';
import { ConversationProvider, useConversationRegistry } from '../conversations/public.tsx';
import { useGo, useRouteHash, useRouteParam } from './navigation.ts';
import { PendingRoute } from './pending-route.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';

export const APP_BASEPATH = '/next';

type ConversationStore = Readonly<{
  conversations: readonly Conversation[];
  /** Messages *and* the actions between them, in the order they happened. */
  turnsOf: (conversationId: string) => readonly TranscriptEntry[];
  pending: ReadonlySet<string>;
  working: boolean;
  stopping: boolean;
  sending: boolean;
  resetting: boolean;
  hasEarlier: boolean;
  loadingEarlier: boolean;
  historyError: string | null;
  actionError: string | null;
  actionMessage: string | null;
  start: () => Conversation | null;
  send: (conversationId: string, text: string) => void;
  interrupt: () => void;
  reset: () => Promise<boolean>;
  loadEarlier: () => void;
}>;

type ConversationRouteIntent = Readonly<
  { kind: 'all' } | { kind: 'waves'; waveIds: readonly string[] }
>;

export function pendingConversationIds(
  conversation: Conversation | null, working: boolean, sending: boolean,
): ReadonlySet<string> {
  return (working || sending) && conversation !== null ? new Set([conversation.id]) : new Set();
}

function sameConversationTurns(
  left: readonly ConversationTurn[], right: readonly ConversationTurn[],
): boolean {
  return left.length === right.length && left.every((turn, index) => {
    const other = right[index];
    return other !== undefined && turn.id === other.id && turn.author === other.author
      && turn.text === other.text && turn.atMs === other.atMs;
  });
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
  const history = useInfiniteQuery({
    ...harnessItemsQueryOptions(transport, cardId, unauthorized), enabled: scope !== null,
  });
  const run = useQuery({ ...specRunQueryOptions(transport, cardId, unauthorized), enabled: scope !== null });
  const mutations = useSpecMutations(transport, cardId, unauthorized);
  const [echoes, setEchoes] = useState<readonly ConversationTurn[]>([]);
  const [sending, setSending] = useState(false);
  const [interruptPending, setInterruptPending] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionMessage, setActionMessage] = useState<string | null>(null);
  const sendingRef = useRef(false);
  const suppressRememberRef = useRef(false);
  const suppressedRememberSnapshotRef = useRef<readonly ConversationTurn[] | null>(null);
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
    setActionMessage(null);
    sendingRef.current = false;
    suppressRememberRef.current = false;
    suppressedRememberSnapshotRef.current = null;
    setSending(false);
    setInterruptPending(false);
    setResetting(false);
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
    id: cardId, waveId, waveTitle: waveTitle ?? '',
    title: cardTitle ?? conversationNameFrom(turns.find((turn) => turn.author === 'you')?.text ?? ''),
    kind: 'shared-spec', state: working ? 'running' : 'idle',
    updatedAt: turns.at(-1)?.atMs ?? scopeUpdatedAt ?? 0, turns: turns.length,
  }, [cardId, cardTitle, scopeUpdatedAt, turns, waveId, waveTitle, working]);
  useEffect(() => {
    if (conversation === null) return;
    if (suppressRememberRef.current && suppressedRememberSnapshotRef.current !== null
      && sameConversationTurns(serverTurns, suppressedRememberSnapshotRef.current)) return;
    suppressRememberRef.current = false;
    suppressedRememberSnapshotRef.current = null;
    // Remember the full transcript so reopening the conversation preserves its
    // activity lines and looks identical to the route the user just left.
    registry.remember(conversation, transcript);
  }, [conversation, registry, serverTurns, transcript]);

  const allConversations = conversation === null
    ? registry.conversations
    : [...registry.conversations.filter(({ id }) => id !== conversation.id), conversation];
  const waveIds = routeIntent.kind === 'waves' ? new Set(routeIntent.waveIds) : null;
  const conversations = waveIds === null
    ? allConversations
    : allConversations.filter((candidate) => waveIds.has(candidate.waveId));

  const send = (_conversationId: string, text: string) => {
    if (sendingRef.current) return;
    sendingRef.current = true;
    setSending(true);
    setActionError(null);
    setActionMessage(null);
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

  const interrupt = () => {
    if (!working || stopping) return;
    setInterruptPending(true);
    setActionError(null);
    void mutations.interrupt().then((result) => {
      if (result.stopped) setActionMessage('Turn stopped');
    }).catch((error: unknown) => {
      setActionError(errorMessage(error, 'Could not stop the turn.'));
    }).finally(() => setInterruptPending(false));
  };

  const reset = async (): Promise<boolean> => {
    if (resetting) return false;
    setResetting(true);
    setActionError(null);
    suppressRememberRef.current = true;
    suppressedRememberSnapshotRef.current = serverTurns;
    if (conversation !== null) registry.forget(conversation.id);
    try {
      await mutations.reset();
      setEchoes([]);
      setActionMessage(null);
      return true;
    } catch (error: unknown) {
      suppressRememberRef.current = false;
      suppressedRememberSnapshotRef.current = null;
      setActionError(errorMessage(error, 'Could not reset the conversation.'));
      return false;
    } finally {
      setResetting(false);
    }
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
    resetting,
    hasEarlier: history.hasNextPage,
    loadingEarlier: history.isFetchingNextPage,
    historyError: history.error instanceof Error ? history.error.message : null,
    actionError,
    actionMessage,
    start: () => conversation,
    send,
    interrupt,
    reset,
    loadEarlier: () => { void history.fetchNextPage().catch(() => undefined); },
  };
}

type SpecConversationScope = Readonly<{
  id: string; title: string; cardId: string; cardTitle: string | null; updatedAt: number;
}>;

export type AppRouterDeps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  client: QueryClient;
  onSignOut: () => void;
}>;

export function createRouteTree({ transport, unauthorized, client, onSignOut }: AppRouterDeps): AnyRoute {
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
    component: () => <WaveRoute transport={transport} unauthorized={unauthorized} />,
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

/**
 * The conversation module, identical on all three routes: a list, a `+` in the
 * module head, and the drawer both of them open.
 *
 * `'new'` is a third open state, not a flag on the second. Starting a
 * conversation and reading one land in the same place — the drawer is *where a
 * conversation is*, so a new one has nowhere else to go — but they are not the
 * same object, and modelling "new" as a null conversation would put an
 * `open === null` check in every branch that reads one.
 */
function useConversationPanel(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
  scope: SpecConversationScope | null,
  routeIntent: ConversationRouteIntent,
  options?: { showWave?: boolean },
) {
  const store = useConversationStore(transport, unauthorized, scope, routeIntent);
  const registry = useConversationRegistry();
  const go = useGo();
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirmingReset, setConfirmingReset] = useState(false);
  const open = store.conversations.find((conversation) => conversation.id === openId) ?? null;

  useEffect(() => {
    if (scope === null || registry.requestedOpenId !== scope.cardId) return;
    setOpenId(scope.cardId);
    registry.clearOpenRequest();
  }, [registry, scope]);

  useEffect(() => {
    if (open === null) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
      if (confirmingReset) {
        event.preventDefault();
        event.stopImmediatePropagation();
        if (!store.resetting) setConfirmingReset(false);
        return;
      }
      if (!store.working || store.stopping) return;
      const target = event.target;
      if (!(target instanceof Element) || target.closest('[role="complementary"]') === null) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      store.interrupt();
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, [confirmingReset, open, store]);

  /*
   * The `+` starts a conversation *and* opens it. A control that creates a row
   * you then have to find and click is two steps for one intention — and on a
   * route with no wave in scope (Today) there is nothing to attach one to, so
   * the action is simply not offered there rather than offered and refused.
   */
  const start = () => {
    if (scope === null) return;
    const conversation = store.start();
    if (conversation !== null) setOpenId(conversation.id);
  };

  return {
    list: (
      <ChatList
        conversations={store.conversations}
        activeId={open?.id ?? null}
        showWave={options?.showWave ?? true}
        onOpen={(conversation) => {
          if (scope !== null) {
            setOpenId(conversation.id);
            return;
          }
          registry.requestOpen(conversation.id);
          go({ name: 'wave', waveId: conversation.waveId });
        }}
      />
    ),
    /* The module head's action, composed by the page — same slot the WAVES and
       CARDS modules already use, which is why this needed no new mechanism. */
    action: scope === null
      ? undefined
      : <PanelAction label="New conversation" onClick={start}>+</PanelAction>,
    drawer: (
      <>
      <Drawer
        open={open !== null}
        title={open === null ? '' : conversationName(open)}
        onClose={() => setOpenId(null)}
        /*
         * Reset lives in the head, not under the composer.
         *
         * It is the *rarest* control on the surface and the only destructive
         * one — it throws the transcript away and starts a new codex thread —
         * and it was sitting beside the message box at the same weight as
         * `Stop`, which you press casually. `destructive` is §4.3's tier:
         * `--error-text` at rest, transparent fill, red before the pointer
         * arrives rather than after. The confirm dialog it opens is unchanged.
         *
         * It is not offered when there is nothing to reset.
         */
        headAction={open === null ? undefined : (
          /* DrawerAction owns the box; the shared icon owns glyph geometry. */
          <DrawerAction danger label="Reset conversation" onClick={() => setConfirmingReset(true)}>
            <Icon name="reset" />
          </DrawerAction>
        )}
        footer={open === null ? undefined : (
          <>
            <ChatComposer disabled={store.sending} onSend={(text) => store.send(open.id, text)} />
            {(store.working || store.stopping) && (
              <button type="button" disabled={store.stopping} onClick={store.interrupt}>
                {store.stopping ? 'Stopping…' : 'Stop'}
              </button>
            )}
            {store.actionError !== null && <p role="alert">{store.actionError}</p>}
            {store.actionMessage !== null && <p role="status">{store.actionMessage}</p>}
          </>
        )}
      >
        {open !== null && (
          <>
            {store.hasEarlier && (
              <button type="button" disabled={store.loadingEarlier} onClick={store.loadEarlier}>
                {store.loadingEarlier ? 'Loading…' : 'Load earlier'}
              </button>
            )}
            {store.historyError !== null && <p role="alert">{store.historyError}</p>}
            <ChatThread
              conversation={open}
              turns={store.turnsOf(open.id)}
              pending={store.pending.has(open.id)}
            />
          </>
        )}
      </Drawer>
      <ConfirmDialog
        open={confirmingReset}
        title="Reset conversation?"
        description={<>
          <p>This clears the transcript and starts a new agent thread. This cannot be undone.</p>
          {store.actionError !== null && <p role="alert">{store.actionError}</p>}
        </>}
        confirmLabel="Reset conversation"
        confirmBusyLabel="Resetting…"
        confirmState={store.resetting ? 'busy' : 'ready'}
        onCancel={() => { if (!store.resetting) setConfirmingReset(false); }}
        onConfirm={() => {
          void store.reset().then((succeeded) => { if (succeeded) setConfirmingReset(false); });
        }}
      />
      </>
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
  const chat = useConversationPanel(transport, unauthorized, null, { kind: 'all' });
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
  const [creating, setCreating] = useState(false);
  /* No `+`: a conversation attaches to a wave (the kernel's sessions hang off
     a card, and cards belong to waves), and this route has no single wave in
     scope. The module still lists and still opens — it is the starting that
     needs somewhere to attach. */
  const coveWaveIds = (workspace.wavesByCove.get(coveId ?? '') ?? []).map(({ id }) => id);
  const chat = useConversationPanel(
    transport, unauthorized, null, { kind: 'waves', waveIds: coveWaveIds },
  );
  const [submitting, setSubmitting] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

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
  const submit = (draft: NewWaveDraft) => {
    setSubmitting(true);
    setCreateError(null);
    void waveMutations.create({
      cove_id: draft.coveId,
      title: draft.title,
      cwd: draft.cwd,
      theme: readHostThemeRgb(),
      attach_folder: draft.attachFolder,
    }).then((wave) => {
      setCreating(false);
      go({ name: 'wave', waveId: wave.id });
    }).catch((error: unknown) => {
      // The 409 body names the cove the directory would have to be claimed
      // for, so the raw server sentence is more useful than a generic line.
      setCreateError(error instanceof ApiError ? error.message : 'Could not create the wave.');
    }).finally(() => { setSubmitting(false); });
  };

  return (
    <>
      {waveError !== null && waveError !== undefined && <ErrorBox
        message={waveError.message}
        onRetry={() => { workspace.retryWaves(cove.id); workspace.retryOverlays(); }}
      />}
      {workspace.overlaysError !== null && <ErrorBox message={`Wave activity is unavailable: ${workspace.overlaysError.message}`} onRetry={workspace.retryOverlays} />}
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
        onRequestNewWave={() => { setCreateError(null); setCreating(true); }}
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
      <Dialog open={creating} onClose={() => setCreating(false)} title="New wave">
        <NewWaveForm
          coves={workspace.coves}
          defaultCoveId={cove.id}
          submitting={submitting}
          error={createError}
          onCancel={() => setCreating(false)}
          onSubmit={submit}
        />
      </Dialog>
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
function WaveRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const waveId = useRouteParam('/wave/');
  const workspace = useWorkspace(transport, unauthorized);
  const registry = useConversationRegistry();
  const detail = useQuery({
    ...waveDetailQueryOptions(transport, waveId ?? '', unauthorized),
    enabled: waveId !== undefined,
  });
  const requestedCard = detail.data?.cards.find((card) => card.id === registry.requestedOpenId
    && card.kind === 'codex'
    && typeof card.payload === 'object' && card.payload !== null
    && (card.payload as { spec_harness?: unknown }).spec_harness === true);
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
    />
  );
}

function WaveRouteBody({ transport, unauthorized, wave, cove, cards }: {
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  wave: Wave;
  cove: Cove | undefined;
  cards: WaveDetailWire['cards'];
}) {
  const waveMutations = useWaveMutations(transport, unauthorized);
  const go = useGo();
  // `showWave: false` — on a wave's own page the wave's name is the page title,
  // so repeating it on every row is one column spent saying nothing.
  const specCard = cards.find((card) => card.kind === 'codex' &&
    typeof card.payload === 'object' && card.payload !== null &&
    (card.payload as { spec_harness?: unknown }).spec_harness === true);
  const chat = useConversationPanel(
    transport,
    unauthorized,
    specCard === undefined ? null : {
      id: wave.id, title: waveDisplayTitle(wave.title), cardId: specCard.id,
      cardTitle: specCard.title, updatedAt: specCard.updated_at,
    },
    { kind: 'waves', waveIds: [wave.id] },
    { showWave: false },
  );
  const { report, outline } = useMemo(() => {
    const nextReport = readWaveReport(cards);
    return {
      report: nextReport,
      outline: deriveReportOutline(nextReport?.blocks ?? null),
    };
  }, [cards]);
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
      go({ name: 'wave', waveId: target.waveId, blockId: target.blockId ?? undefined });
      return;
    }
    go({ name: 'wave', waveId: target.waveId, blockId: target.blockId ?? undefined });
  };

  return (
    <>
    <WavePage
      wave={wave}
      cards={cards}
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
            onOpen={(waveId, blockId) => { go({ name: 'wave', waveId, blockId }); }}
          />
        )
        : undefined}
      conversationList={chat.list}
      conversationAction={chat.action}
      onRenameWave={(title) => waveMutations.patch(wave.id, wave.coveId, { title }).then(() => undefined)}
      onDeleteWave={(signal) => waveMutations.remove(wave.id, wave.coveId, signal).then(() => {
        if (signal.aborted) return;
        if (cove !== undefined) go({ name: 'cove', coveId: cove.id });
        else go({ name: 'today' });
      })}
    />
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
