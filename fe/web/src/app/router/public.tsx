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
import { useRef } from 'react';
import { useQuery, type QueryClient } from '@tanstack/react-query';

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
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ReportEmpty } from '../../features/report/empty/public.tsx';
import { readWaveReport } from '../../../../core/domain/report.ts';
import type { Conversation, ConversationTurn } from '../../../../core/domain/conversation.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { Drawer } from '../../ui/drawer/public.tsx';
import { PanelAction } from '../../ui/panel-card/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  ApiError, prefetchCoveList, settingsQueryOptions, useCoveMutations, useSettingsMutation,
  useWaveMutations, useWorkspace, waveDetailQueryOptions,
} from '../providers/queries.ts';
import { AppShell } from '../shell/public.tsx';
import { useTheme } from '../theme/public.tsx';
import { useGo, useRouteParam } from './navigation.ts';
import { PendingRoute } from './pending-route.tsx';

/**
 * The conversation store, standing in for an endpoint that does not exist.
 *
 * `core/domain/conversation.ts` explains what the kernel really holds — a
 * `WorkerSessionProjection` per session and `HarnessItem` rows for its turns —
 * and what it does not have: any HTTP route the frontend can read them from.
 * So the *composition layer* holds them, in memory, for the length of a visit.
 *
 * It lives here rather than in `features/chat` on purpose. Nothing under
 * `features/**` learns that its data is a stub: the list takes a list, the
 * thread takes turns, the composer reports a string. When the endpoint lands,
 * this hook becomes a query plus a mutation and not one line of the chat
 * feature changes. A stub inside the feature would have had to be unpicked
 * from it instead.
 *
 * The reply is the literal string `test`, which is what was asked for and is
 * also the honest thing to render: an agent is not attached, and a stub that
 * wrote something plausible would be claiming one is.
 */
const STUB_REPLY = 'test';
const STUB_REPLY_DELAY_MS = 400;

type ConversationStore = Readonly<{
  conversations: readonly Conversation[];
  turnsOf: (conversationId: string) => readonly ConversationTurn[];
  pending: string | null;
  start: (wave: { id: string; title: string }) => Conversation;
  send: (conversationId: string, text: string) => void;
}>;

function useConversationStore(): ConversationStore {
  const [conversations, setConversations] = useState<readonly Conversation[]>([]);
  const [turns, setTurns] = useState<Readonly<Record<string, readonly ConversationTurn[]>>>({});
  const [pending, setPending] = useState<string | null>(null);
  const seq = useRef(0);
  const nextId = (prefix: string) => { seq.current += 1; return `${prefix}-${seq.current}`; };

  const start = (wave: { id: string; title: string }): Conversation => {
    const now = Date.now();
    const conversation: Conversation = {
      id: nextId('conv'), waveId: wave.id, waveTitle: wave.title,
      kind: 'codex', state: 'idle', updatedAt: now, turns: 0,
    };
    setConversations((current) => [conversation, ...current]);
    return conversation;
  };

  const append = (conversationId: string, turn: ConversationTurn) => {
    setTurns((current) => ({ ...current, [conversationId]: [...(current[conversationId] ?? []), turn] }));
    setConversations((current) => current.map((conversation) => (
      conversation.id === conversationId
        ? { ...conversation, updatedAt: turn.atMs, turns: conversation.turns + 1 }
        : conversation
    )));
  };

  const send = (conversationId: string, text: string) => {
    append(conversationId, { id: nextId('turn'), author: 'you', text, atMs: Date.now() });
    setPending(conversationId);
    // A delay, because the state it puts the surface in is real: the live dot
    // and the "working" state exist and have to be reachable to be looked at.
    window.setTimeout(() => {
      append(conversationId, { id: nextId('turn'), author: 'agent', text: STUB_REPLY, atMs: Date.now() });
      setPending(null);
    }, STUB_REPLY_DELAY_MS);
  };

  return {
    conversations,
    turnsOf: (conversationId) => turns[conversationId] ?? EMPTY_TURNS,
    pending,
    start,
    send,
  };
}

/** One frozen reference, so a conversation with no turns does not hand React a
 *  new array on every render. */
const EMPTY_TURNS: readonly ConversationTurn[] = Object.freeze([]);

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
 * The conversation module every route's panel card carries, plus the drawer it
 * opens into.
 *
 * It lives here, in the composition layer, for two separate reasons. The list
 * is `features/chat` and the pages are `features/today|cove|wave`, and a
 * feature may not import a sibling domain — so someone above them has to put
 * the two together, exactly as `renderWaveRow` and `waveList` already are. And
 * the drawer overlays the entire main region (§7.6), which is not something a
 * 308px module inside one page should own.
 *
 * `conversations` is empty in production: the kernel holds this data
 * (`WorkerSessionProjection` + `HarnessItem`) but no HTTP endpoint serves it
 * yet, so the list renders §5.3's unbuilt shape. See
 * `core/domain/conversation.ts`.
 */
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
  scope: { id: string; title: string } | null,
  options?: { showWave?: boolean },
) {
  const store = useConversationStore();
  const [openId, setOpenId] = useState<string | null>(null);
  const open = store.conversations.find((conversation) => conversation.id === openId) ?? null;

  /*
   * The `+` starts a conversation *and* opens it. A control that creates a row
   * you then have to find and click is two steps for one intention — and on a
   * route with no wave in scope (Today) there is nothing to attach one to, so
   * the action is simply not offered there rather than offered and refused.
   */
  const start = () => {
    if (scope === null) return;
    setOpenId(store.start(scope).id);
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
      <Drawer
        open={open !== null}
        title={open?.waveTitle ?? ''}
        onClose={() => setOpenId(null)}
        footer={open === null ? undefined : (
          <ChatComposer onSend={(text) => store.send(open.id, text)} />
        )}
      >
        {open !== null && (
          <ChatThread
            conversation={open}
            turns={store.turnsOf(open.id)}
            pending={store.pending === open.id}
          />
        )}
      </Drawer>
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
  const chat = useConversationPanel(null);
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
  const chat = useConversationPanel(null);
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
  const chat = useConversationPanel(
    { id: wave.id, title: waveDisplayTitle(wave.title) },
    { showWave: false },
  );

  return (
    <>
    <WavePage
      wave={wave}
      cards={cards}
      report={<ReportDocument
        body={readWaveReport(cards)?.body ?? null}
        empty={<ReportEmpty
          lead="Nothing written here yet."
          hints={[
            'The agent writes this report as it works — start a conversation and it fills in.',
            'It stays with the wave, so it is here the next time you open it.',
          ]}
        />}
      />}
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
