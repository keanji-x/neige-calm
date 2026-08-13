// The layout shell every route renders inside: the workspace rail plus the
// matched route's outlet.
//
// The shell owns the workspace read *and* the cove/wave mutations, and hands
// the rail plain callbacks: `Sidebar` stays presentational, so a test can drive
// it without a QueryClient. Sign-out is not implemented here — whoever owns the
// session passes it in.

import { Outlet } from '@tanstack/react-router';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { useState } from '../../ui/state/public.ts';
import { useCoveMutations, useWaveMutations, useWorkspace } from '../providers/queries.ts';
import { useCurrentPath, useGo } from '../router/navigation.ts';
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

export function AppShell({ transport, onOpenSettings, onSignOut, nowMs, userLabel }: AppShellProps) {
  const workspace = useWorkspace(transport);
  const coveMutations = useCoveMutations(transport);
  const waveMutations = useWaveMutations(transport);
  const currentPath = useCurrentPath();
  const go = useGo();
  const readError = workspace.covesError
    ?? workspace.waveErrorsByCove.values().next().value ?? null;

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

  // Both wave mutations need the cove id to invalidate the right list, and the
  // rail only knows wave ids; the workspace read already has the mapping.
  const coveIdOf = (waveId: string): string | undefined =>
    workspace.waves.find((wave) => wave.id === waveId)?.coveId;

  return (
    <div className={`${styles.shell} ${railCollapsed ? styles.shellCollapsed : ''}`}>
      <Sidebar
        collapsed={railCollapsed}
        onToggleCollapsed={() => setRailCollapsed(!railCollapsed)}
        coves={workspace.coves}
        wavesByCove={workspace.wavesByCove}
        waves={workspace.waves}
        currentPath={currentPath}
        readError={readError?.message ?? null}
        readLoading={workspace.covesLoading || workspace.overlaysLoading
          || [...workspace.wavesLoadingByCove.values()].some(Boolean)}
        activityError={workspace.overlaysError?.message ?? null}
        onRetryRead={() => {
          workspace.retryCoves(); workspace.retryOverlays();
          for (const cove of workspace.coves) workspace.retryWaves(cove.id);
        }}
        onGo={go}
        onCreateCove={async (name, color) => { await coveMutations.create({ name, color }); }}
        onDeleteCove={(coveId, signal) => coveMutations.remove(coveId, signal)}
        onSetPinned={async (waveId, pinned) => {
          const coveId = coveIdOf(waveId);
          if (coveId === undefined) return;
          await waveMutations.setPinned(waveId, coveId, pinned, nowMs ?? Date.now());
        }}
        onDeleteWave={async (waveId, signal) => {
          const coveId = coveIdOf(waveId);
          if (coveId === undefined) return;
          await waveMutations.remove(waveId, coveId, signal);
        }}
        onOpenSettings={onOpenSettings}
        onSignOut={onSignOut}
        userLabel={userLabel}
      />
      <main className={styles.main}>
        <Outlet />
      </main>
    </div>
  );
}
