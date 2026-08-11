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
import { useQuery, type QueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { coveOf } from '../../../../core/domain/cove.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { CovePage } from '../../features/cove/page/public.tsx';
import { NewWaveForm, type NewWaveDraft } from '../../features/cove/new-wave/public.tsx';
import { SettingsPage, type ThemeMode as SettingsThemeMode } from '../../features/settings/public.tsx';
import { TodayPage } from '../../features/today/public.tsx';
import { WaveList } from '../../features/wave/list/public.tsx';
import { WaveRow } from '../../features/wave/row/public.tsx';
import { WavePage } from '../../features/wave/page/public.tsx';
import { ChatList } from '../../features/chat/list/public.tsx';
import type { Conversation } from '../../../../core/domain/conversation.ts';
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
import paneStyles from './router.module.css';

/**
 * A frozen empty list, so every route hands the same reference down and React
 * cannot see a "new" array on each render. Production has no conversation
 * endpoint yet; see `core/domain/conversation.ts`.
 */
const NO_CONVERSATIONS: readonly Conversation[] = Object.freeze([]);

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
function useConversationPanel(conversations: readonly Conversation[], options?: { showWave?: boolean }) {
  const [open, setOpen] = useState<Conversation | 'new' | null>(null);
  const current = open === 'new' ? null : open;
  return {
    list: (
      <ChatList
        conversations={conversations}
        activeId={current?.id ?? null}
        showWave={options?.showWave ?? true}
        onOpen={setOpen}
      />
    ),
    /* The module head's action, composed by the page — same slot the WAVES and
       CARDS modules already use, which is why this needed no new mechanism. */
    action: <PanelAction label="New conversation" onClick={() => setOpen('new')}>+</PanelAction>,
    drawer: (
      <Drawer
        open={open !== null}
        title={open === 'new' ? 'New conversation' : open?.waveTitle ?? ''}
        onClose={() => setOpen(null)}
      >
        {/* The transcript is the same unbuilt story as the list: the turns
            exist in the kernel, the endpoint does not. The drawer still opens,
            at the width and behaviour §7.6 fixed, so the shape is real even
            though the content is not.

            The `+` therefore opens a real drawer that says plainly it cannot
            send yet, rather than a button that does nothing when clicked. A
            control that no-ops is worse than one that tells you why. */}
        <p className={paneStyles.drawerNote}>
          {open === 'new' ? 'Sending is not wired up yet.' : 'No transcript yet.'}
        </p>
      </Drawer>
    ),
  };
}

function TodayRoute({ transport }: { transport: ApiTransportPort }) {
  const workspace = useWorkspace(transport);
  const go = useGo();
  const chat = useConversationPanel(NO_CONVERSATIONS);
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
  const chat = useConversationPanel(NO_CONVERSATIONS);
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

function WaveRoute({ transport }: { transport: ApiTransportPort }) {
  const waveId = useRouteParam('/wave/');
  const workspace = useWorkspace(transport);
  // `showWave: false` — on a wave's own page the wave's name is the page title,
  // so repeating it on every row is one column spent saying nothing.
  const chat = useConversationPanel(NO_CONVERSATIONS, { showWave: false });
  const waveMutations = useWaveMutations(transport);
  const go = useGo();
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
  const cove = coveOf(wave.coveId, workspace.coves);

  return (
    <>
    <WavePage
      wave={wave}
      cards={detail.data.cards}
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
