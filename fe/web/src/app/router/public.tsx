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
import { WavePage } from '../../features/wave/page/public.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  ApiError, prefetchCoveList, settingsQueryOptions, useCoveMutations, useSettingsMutation,
  useWaveMutations, useWorkspace, waveDetailQueryOptions,
} from '../providers/queries.ts';
import { AppShell } from '../shell/public.tsx';
import { useTheme } from '../theme/public.tsx';
import { useGo, useRouteParam } from './navigation.ts';
import { PendingRoute } from './pending-route.tsx';

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

function TodayRoute({ transport }: { transport: ApiTransportPort }) {
  const workspace = useWorkspace(transport);
  const go = useGo();
  return (
    <TodayPage
      waves={workspace.waves}
      coves={workspace.coves}
      onOpenWave={(waveId) => go({ name: 'wave', waveId })}
    />
  );
}

function CoveRoute({ transport }: { transport: ApiTransportPort }) {
  const coveId = useRouteParam('/cove/');
  const workspace = useWorkspace(transport);
  const coveMutations = useCoveMutations(transport);
  const waveMutations = useWaveMutations(transport);
  const go = useGo();
  const [creating, setCreating] = useState(false);
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
        waveList={(
          <WaveList
            waves={waves}
            coves={workspace.coves}
            emptyMessage="This cove is quiet. Start a wave."
            onOpenWave={(waveId) => go({ name: 'wave', waveId })}
            onSetPinned={(waveId, pinned) => {
              void waveMutations.setPinned(waveId, cove.id, pinned, Date.now());
            }}
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
    </>
  );
}

function WaveRoute({ transport }: { transport: ApiTransportPort }) {
  const waveId = useRouteParam('/wave/');
  const workspace = useWorkspace(transport);
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
    <WavePage
      wave={wave}
      cove={cove}
      cards={detail.data.cards}
      onOpenCove={() => { if (cove !== undefined) go({ name: 'cove', coveId: cove.id }); }}
      onOpenToday={() => go({ name: 'today' })}
      onRenameWave={(title) => waveMutations.patch(wave.id, wave.coveId, { title }).then(() => undefined)}
      onDeleteWave={() => waveMutations.remove(wave.id, wave.coveId).then(() => {
        if (cove !== undefined) go({ name: 'cove', coveId: cove.id });
        else go({ name: 'today' });
      })}
    />
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
