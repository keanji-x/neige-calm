// The layout shell every route renders inside: the workspace rail plus the
// matched route's outlet.
//
// The shell owns the workspace read *and* the cove/wave mutations, and hands
// the rail plain callbacks: `Sidebar` stays presentational, so a test can drive
// it without a QueryClient. Sign-out is not implemented here — whoever owns the
// session passes it in.

import { Outlet } from '@tanstack/react-router';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
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

  // Both wave mutations need the cove id to invalidate the right list, and the
  // rail only knows wave ids; the workspace read already has the mapping.
  const coveIdOf = (waveId: string): string | undefined =>
    workspace.waves.find((wave) => wave.id === waveId)?.coveId;

  return (
    <div className={styles.shell}>
      <Sidebar
        coves={workspace.coves}
        wavesByCove={workspace.wavesByCove}
        waves={workspace.waves}
        currentPath={currentPath}
        onGo={go}
        onCreateCove={async (name, color) => { await coveMutations.create({ name, color }); }}
        onDeleteCove={(coveId) => coveMutations.remove(coveId)}
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
      <main className={styles.main}>
        <Outlet />
      </main>
    </div>
  );
}
