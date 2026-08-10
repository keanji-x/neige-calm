// The layout shell every route renders inside: the workspace rail plus the
// matched route's outlet.

import { Outlet } from '@tanstack/react-router';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { useWorkspace } from '../providers/queries.ts';
import { useCurrentPath, useGo } from '../router/navigation.ts';
import { Sidebar } from './sidebar.tsx';
import styles from './shell.module.css';

export function AppShell({ transport }: { transport: ApiTransportPort }) {
  const workspace = useWorkspace(transport);
  const currentPath = useCurrentPath();
  const go = useGo();

  return (
    <div className={styles.shell}>
      <Sidebar
        coves={workspace.coves}
        wavesByCove={workspace.wavesByCove}
        waves={workspace.waves}
        currentPath={currentPath}
        onGo={go}
      />
      <main className={styles.main}>
        <Outlet />
      </main>
    </div>
  );
}
