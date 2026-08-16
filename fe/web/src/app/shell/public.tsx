// The layout shell every route renders inside: the workspace rail plus the
// matched route's outlet.
//
// The shell owns the workspace read *and* the cove/wave mutations, and hands
// the rail plain callbacks: `Sidebar` stays presentational, so a test can drive
// it without a QueryClient. Sign-out is not implemented here — whoever owns the
// session passes it in.
//
// It also owns the **New wave dialog**, for the same reason it owns the
// mutations: two surfaces open it — every cove row's `+` in the rail, and the
// cove page's WAVES module head — and they must open one dialog with one set of
// strings. The rail is a sibling of the outlet, so a dialog living inside the
// cove route was unreachable from it; the shell is the nearest place that sees
// both, and it already holds `useWorkspace` + `useWaveMutations`, which is
// everything the dialog needs.

import { Outlet } from '@tanstack/react-router';
import { createContext, useContext } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { NewWaveForm, type NewWaveDraft } from '../../features/cove/new-wave/public.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  ApiError, useCoveFolders, useCoveMutations, useWaveMutations, useWorkspace,
} from '../providers/queries.ts';
import { useCurrentPath, useGo } from '../router/navigation.ts';
import { readHostThemeRgb } from '../theme/host-rgb.ts';
import { Sidebar } from './sidebar.tsx';
import styles from './shell.module.css';

export type AppShellProps = Readonly<{
  transport: ApiTransportPort;
  onOpenSettings: () => void;
  onSignOut: () => void;
  /** Pinned by tests so `pinned_at` assertions are stable. */
  nowMs?: number;
  userLabel?: string;
}>;

/**
 * The one escape a route has into the shell's dialog.
 *
 * A context and not a prop because the cove route renders inside `<Outlet />`:
 * there is no prop path from here to it, and the alternative — a second dialog
 * inside the route — is the thing this change removed. It carries a single
 * callback, so a consumer cannot come to depend on the shell's internals.
 */
const RequestNewWaveContext = createContext<((coveId: string) => void) | null>(null);

/** Opens the shell's New wave dialog with `coveId` preselected. */
export function useRequestNewWave(): (coveId: string) => void {
  const request = useContext(RequestNewWaveContext);
  // Outside the shell there is no dialog to open. Routes always render inside
  // it; a no-op keeps a stray consumer (a test rendering a page bare) from
  // throwing on a control it is not exercising.
  return request ?? noRequestNewWave;
}

function noRequestNewWave(): void { /* no shell above this consumer */ }

export function AppShell({ transport, onOpenSettings, onSignOut, nowMs, userLabel }: AppShellProps) {
  const workspace = useWorkspace(transport);
  const coveMutations = useCoveMutations(transport);
  const waveMutations = useWaveMutations(transport);
  const currentPath = useCurrentPath();
  const go = useGo();

  /*
   * The collapsed flag lives here, not inside `Sidebar`, because collapsing is
   * a *grid* change: the rail may swap its contents for an icon strip, but
   * unless this element's `grid-template-columns` also changes, the column
   * stays 200px wide and the button appears to do nothing. That was the bug.
   *
   * Manual choice always wins (§7.1). The sub-960px auto-collapse is a media
   * query on the same grid and deliberately does not write this state, so
   * widening the window restores whatever the user picked.
   */
  const [railCollapsed, setRailCollapsed] = useState(false);

  /*
   * One state, and `null` is closed. The cove the dialog is *for* and the cove
   * its select currently shows are the same fact — changing the select changes
   * which folders are read — so modelling them separately would only create a
   * pair that can disagree.
   */
  const [newWaveCoveId, setNewWaveCoveId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const coveFolders = useCoveFolders(transport, newWaveCoveId);

  // Both wave mutations need the cove id to invalidate the right list, and the
  // rail only knows wave ids; the workspace read already has the mapping.
  const coveIdOf = (waveId: string): string | undefined =>
    workspace.waves.find((wave) => wave.id === waveId)?.coveId;

  const requestNewWave = (coveId: string) => {
    setCreateError(null);
    setNewWaveCoveId(coveId);
  };

  const submitNewWave = (draft: NewWaveDraft) => {
    setCreating(true);
    setCreateError(null);
    void waveMutations.create({
      cove_id: draft.coveId,
      title: draft.title,
      cwd: draft.cwd,
      theme: readHostThemeRgb(),
      attach_folder: draft.attachFolder,
    }).then((wave) => {
      setNewWaveCoveId(null);
      go({ name: 'wave', waveId: wave.id });
    }).catch((error: unknown) => {
      // The 409 body names the cove the directory would have to be claimed
      // for, so the raw server sentence is more useful than a generic line.
      setCreateError(error instanceof ApiError ? error.message : 'Could not create the wave.');
    }).finally(() => { setCreating(false); });
  };

  return (
    <div className={`${styles.shell} ${railCollapsed ? styles.shellCollapsed : ''}`}>
      <Sidebar
        collapsed={railCollapsed}
        onToggleCollapsed={() => setRailCollapsed(!railCollapsed)}
        coves={workspace.coves}
        wavesByCove={workspace.wavesByCove}
        waves={workspace.waves}
        currentPath={currentPath}
        onGo={go}
        onCreateCove={async (name, color) => { await coveMutations.create({ name, color }); }}
        onDeleteCove={(coveId) => coveMutations.remove(coveId)}
        onNewWave={requestNewWave}
        onSetPinned={async (waveId, pinned) => {
          const coveId = coveIdOf(waveId);
          if (coveId === undefined) return;
          await waveMutations.setPinned(waveId, coveId, pinned, nowMs ?? Date.now());
        }}
        onDeleteWave={async (waveId) => {
          const coveId = coveIdOf(waveId);
          if (coveId === undefined) return;
          await waveMutations.remove(waveId, coveId);
        }}
        onOpenSettings={onOpenSettings}
        onSignOut={onSignOut}
        userLabel={userLabel}
      />
      {/* The provider wraps the outlet only. The rail takes the same callback as
          a plain prop, so `Sidebar` stays presentational and its tests keep
          driving it with no context above them. */}
      <main className={styles.main}>
        <RequestNewWaveContext.Provider value={requestNewWave}>
          <Outlet />
        </RequestNewWaveContext.Provider>
      </main>
      <Dialog open={newWaveCoveId !== null} onClose={() => setNewWaveCoveId(null)} title="New wave">
        {newWaveCoveId !== null && (
          <NewWaveForm
            coves={workspace.coves}
            coveId={newWaveCoveId}
            onCoveIdChange={setNewWaveCoveId}
            folders={coveFolders.folders}
            foldersLoading={coveFolders.loading}
            submitting={creating}
            error={createError}
            onCancel={() => setNewWaveCoveId(null)}
            onSubmit={submitNewWave}
          />
        )}
      </Dialog>
    </div>
  );
}
