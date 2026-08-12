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
import { coveOf, type Cove } from '../../../../core/domain/cove.ts';
import { waveDisplayTitle, type Wave, type WaveDetailWire } from '../../../../core/domain/wave.ts';
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
  conversationName, conversationNameFrom, harnessItemToTurn, reconcileUserEchoes,
  type Conversation, type ConversationTurn,
} from '../../../../core/domain/conversation.ts';
import { ConfirmDialog, Dialog } from '../../ui/dialog/public.tsx';
import { Drawer } from '../../ui/drawer/public.tsx';
import { PanelAction } from '../../ui/panel-card/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  ApiError, harnessItemsQueryOptions, prefetchCoveList, settingsQueryOptions, specRunQueryOptions,
  useCoveMutations, useSettingsMutation, useSpecMutations, useWaveMutations, useWorkspace,
  waveBacklinksQueryOptions, waveDetailQueryOptions,
} from '../providers/queries.ts';
import { AppShell } from '../shell/public.tsx';
import { useTheme } from '../theme/public.tsx';
import { useGo, useRouteHash, useRouteParam } from './navigation.ts';
import { PendingRoute } from './pending-route.tsx';

type ConversationStore = Readonly<{
  conversations: readonly Conversation[];
  turnsOf: (conversationId: string) => readonly ConversationTurn[];
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

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message !== '' ? error.message : fallback;
}

/*
 * Conversation liveness depends on the /api/events stream from #1057 and on
 * its PR-B G4 harness.* invalidation plan. Until both are present, persisted
 * agent replies and harness phase changes do not appear here automatically.
 */
function useConversationStore(transport: ApiTransportPort, scope: SpecConversationScope | null): ConversationStore {
  const cardId = scope?.cardId ?? '';
  const history = useInfiniteQuery({
    ...harnessItemsQueryOptions(transport, cardId), enabled: scope !== null,
  });
  const run = useQuery({ ...specRunQueryOptions(transport, cardId), enabled: scope !== null });
  const mutations = useSpecMutations(transport, cardId);
  const [echoes, setEchoes] = useState<readonly ConversationTurn[]>([]);
  const [sending, setSending] = useState(false);
  const [interruptPending, setInterruptPending] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionMessage, setActionMessage] = useState<string | null>(null);
  const sendingRef = useRef(false);
  const seq = useRef(0);
  const serverTurns = useMemo(() => (history.data?.pages ?? [])
    .flat().sort((left, right) => left.id - right.id).flatMap((item) => {
      const turn = harnessItemToTurn(item);
      return turn === null ? [] : [turn];
    }), [history.data]);
  useEffect(() => {
    setEchoes((current) => reconcileUserEchoes(serverTurns, current));
  }, [serverTurns]);
  useEffect(() => {
    setEchoes([]);
    setActionError(null);
    setActionMessage(null);
    sendingRef.current = false;
    setSending(false);
    setInterruptPending(false);
    setResetting(false);
  }, [cardId]);

  const turns = [...serverTurns, ...echoes].sort((left, right) => left.atMs - right.atMs);
  const phase = run.data?.phase ?? null;
  const working = phase === 'issuing_turn' || phase === 'turn_running';
  const stopping = phase === 'issuing_interrupt' || interruptPending;
  const conversation: Conversation | null = scope === null ? null : {
    id: scope.cardId, waveId: scope.id, waveTitle: scope.title,
    title: scope.cardTitle ?? conversationNameFrom(turns.find((turn) => turn.author === 'you')?.text ?? ''),
    kind: 'shared-spec', state: working ? 'running' : 'idle',
    updatedAt: turns.at(-1)?.atMs ?? scope.updatedAt, turns: turns.length,
  };

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
    try {
      await mutations.reset();
      setEchoes([]);
      setActionMessage(null);
      return true;
    } catch (error: unknown) {
      setActionError(errorMessage(error, 'Could not reset the conversation.'));
      return false;
    } finally {
      setResetting(false);
    }
  };

  return {
    conversations: conversation === null ? [] : [conversation],
    turnsOf: () => turns,
    pending: working && conversation !== null ? new Set([conversation.id]) : new Set(),
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
  client: QueryClient;
  onSignOut: () => void;
}>;

export function createRouteTree({ transport, client, onSignOut }: AppRouterDeps): AnyRoute {
  const rootRoute = createRootRoute({ component: () => <ShellRoute transport={transport} onSignOut={onSignOut} /> });

  const indexRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/',
    /**
     * INV-APP-084 — the index loader primes **only** the coves list. The
     * cove → waves fan-out stays lazy inside the page (`useQueries` in
     * `useWorkspace`); awaiting it here would let one slow cove block the
     * whole calendar behind the route commit.
     */
    loader: () => prefetchCoveList(client, transport),
    component: () => <TodayRoute transport={transport} />,
  });

  const coveRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/cove/$coveId',
    component: () => <CoveRoute transport={transport} />,
  });

  const waveRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/wave/$waveId',
    component: () => <WaveRoute transport={transport} />,
  });

  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings',
    component: () => <SettingsRoute transport={transport} />,
  });

  return rootRoute.addChildren([indexRoute, coveRoute, waveRoute, settingsRoute]);
}

export function createAppRouter(deps: AppRouterDeps) {
  return createRouter({ routeTree: createRouteTree(deps), defaultPreload: false });
}

function ShellRoute({ transport, onSignOut }: { transport: ApiTransportPort; onSignOut: () => void }) {
  const go = useGo();
  return (
    <AppShell
      transport={transport}
      onOpenSettings={() => go({ name: 'settings' })}
      onSignOut={onSignOut}
    />
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
  scope: SpecConversationScope | null,
  options?: { showWave?: boolean },
) {
  const store = useConversationStore(transport, scope);
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirmingReset, setConfirmingReset] = useState(false);
  const open = store.conversations.find((conversation) => conversation.id === openId) ?? null;

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
        onOpen={(conversation) => setOpenId(conversation.id)}
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
        footer={open === null ? undefined : (
          <>
            <ChatComposer disabled={store.sending} onSend={(text) => store.send(open.id, text)} />
            {(store.working || store.stopping) && (
              <button type="button" disabled={store.stopping} onClick={store.interrupt}>
                {store.stopping ? 'Stopping…' : 'Stop'}
              </button>
            )}
            <button type="button" onClick={() => setConfirmingReset(true)}>Reset conversation</button>
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

function TodayRoute({ transport }: { transport: ApiTransportPort }) {
  const workspace = useWorkspace(transport);
  const go = useGo();
  const waveMutations = useWaveMutations(transport);
  /* No `+`: a conversation attaches to a wave (the kernel's sessions hang off
     a card, and cards belong to waves), and this route has no single wave in
     scope. The module still lists and still opens — it is the starting that
     needs somewhere to attach. */
  const chat = useConversationPanel(transport, null);
  return (
    <>
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
          onDelete={options.variant === 'panel'
            ? (waveId) => { void waveMutations.remove(waveId, wave.coveId); }
            : undefined}
        />
      )}
      conversationList={chat.list}
        conversationAction={chat.action}
    />
    {chat.drawer}
    </>
  );
}

function CoveRoute({ transport }: { transport: ApiTransportPort }) {
  const coveId = useRouteParam('/cove/');
  const workspace = useWorkspace(transport);
  const coveMutations = useCoveMutations(transport);
  const waveMutations = useWaveMutations(transport);
  const go = useGo();
  const [creating, setCreating] = useState(false);
  /* No `+`: a conversation attaches to a wave (the kernel's sessions hang off
     a card, and cards belong to waves), and this route has no single wave in
     scope. The module still lists and still opens — it is the starting that
     needs somewhere to attach. */
  const chat = useConversationPanel(transport, null);
  const [submitting, setSubmitting] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const cove = coveId === undefined ? undefined : coveOf(coveId, workspace.coves);
  if (cove === undefined) {
    // While the coves list is still loading we do not know whether the cove
    // exists; showing "missing" first and the real page a moment later reads
    // as a flash of a wrong answer.
    if (workspace.covesLoading) return null;
    return <PendingRoute label="Cove" owner="features/cove" missing />;
  }
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
        onDeleteCove={() => coveMutations.remove(cove.id).then(() => { go({ name: 'today' }); })}
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
            onDeleteWave={(waveId) => { void waveMutations.remove(waveId, cove.id); }}
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
function WaveRoute({ transport }: { transport: ApiTransportPort }) {
  const waveId = useRouteParam('/wave/');
  const workspace = useWorkspace(transport);
  const detail = useQuery({
    ...waveDetailQueryOptions(transport, waveId ?? ''),
    enabled: waveId !== undefined,
  });

  if (!detail.data) {
    if (detail.isLoading || detail.isFetching) return null;
    return <PendingRoute label="Wave" owner="features/wave" missing />;
  }
  // `detail.data` can still be the previously-viewed wave while this one
  // fetches; rendering it under this URL would show the wrong wave.
  if (waveId !== undefined && detail.data.wave.id !== waveId) return null;

  const wave = workspace.waves.find((candidate) => candidate.id === detail.data.wave.id);
  if (wave === undefined) return null;

  return (
    <WaveRouteBody
      key={wave.id}
      transport={transport}
      wave={wave}
      cove={coveOf(wave.coveId, workspace.coves)}
      cards={detail.data.cards}
    />
  );
}

function WaveRouteBody({ transport, wave, cove, cards }: {
  transport: ApiTransportPort;
  wave: Wave;
  cove: Cove | undefined;
  cards: WaveDetailWire['cards'];
}) {
  const waveMutations = useWaveMutations(transport);
  const go = useGo();
  // `showWave: false` — on a wave's own page the wave's name is the page title,
  // so repeating it on every row is one column spent saying nothing.
  const specCard = cards.find((card) => card.kind === 'codex' &&
    typeof card.payload === 'object' && card.payload !== null &&
    (card.payload as { spec_harness?: unknown }).spec_harness === true);
  const chat = useConversationPanel(
    transport,
    specCard === undefined ? null : {
      id: wave.id, title: waveDisplayTitle(wave.title), cardId: specCard.id,
      cardTitle: specCard.title, updatedAt: specCard.updated_at,
    },
    { showWave: false },
  );

  const report = readWaveReport(cards);
  const outline = deriveReportOutline(report?.blocks ?? null);
  const backlinksQuery = useQuery(waveBacklinksQueryOptions(transport, wave.id));
  const backlinks = backlinksQuery.data;

  /*
   * A `neige://wave/…` citation. Same wave — the common case, since a report
   * mostly cites its own sections — is a scroll, not a navigation: routing to
   * the URL you are already on would remount the document and lose the
   * reader's place. Another wave is a real navigation carrying the block in the
   * hash, which is also what makes a pasted deep link land in the right place.
   */
  const arrivalAnchorId = useRouteHash();
  const openReportLink = (target: ReportLinkTarget) => {
    if (target.waveId === wave.id) {
      if (target.blockId !== null) revealReportAnchor(target.blockId);
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
      onDeleteWave={() => waveMutations.remove(wave.id, wave.coveId).then(() => {
        if (cove !== undefined) go({ name: 'cove', coveId: cove.id });
        else go({ name: 'today' });
      })}
    />
    {chat.drawer}
    </>
  );
}

function SettingsRoute({ transport }: { transport: ApiTransportPort }) {
  const go = useGo();
  const theme = useTheme();
  const save = useSettingsMutation(transport);
  const settings = useQuery(settingsQueryOptions(transport));
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<number | null>(null);

  return (
    <SettingsPage
      settings={settings.data?.settings}
      loadError={settings.error instanceof Error ? settings.error.message : null}
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
