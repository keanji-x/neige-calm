import type { ReactNode } from 'react';
import type { SupervisorSnapshot } from '../../../../core/domain/daily-planner.ts';
import { DatabaseBlock, DeploymentBlock, FindingsBlock, InspectionBlock, ResourcesBlock, TrackHealthBlock } from './supervisor-blocks.tsx';
import styles from './supervisor-dashboard.module.css';

function observationTime(value: number): string {
  return new Date(value).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false });
}

export function SupervisorDashboard({ snapshot, status }: Readonly<{
  snapshot: SupervisorSnapshot | undefined;
  status: ReactNode;
}>) {
  const oldestRetirement = snapshot?.retired_workspaces?.oldest_retired_at_ms;
  return <section className={styles.dashboard} aria-label="Daily Supervisor">
    <header className={styles.heading}>
      <h2>Daily Supervisor</h2>
      <p>{snapshot === undefined ? '等待实例观测' : `观测于 ${observationTime(snapshot.observed_at_ms)} · Asia/Shanghai`}</p>
    </header>
    {status}
    <FindingsBlock />
    <div className={styles.primary}>
      <DeploymentBlock version={snapshot?.kernel_version} commit={snapshot?.build_sha} />
      <DatabaseBlock probe={snapshot?.database_probe_ms} />
      <TrackHealthBlock bootCompleted={snapshot?.scheduler_boot_sweep_completed} />
    </div>
    <div className={styles.secondary}>
      <ResourcesBlock count={snapshot?.retired_workspaces?.count}
        oldestRetirement={oldestRetirement == null ? undefined : observationTime(oldestRetirement)} />
      <InspectionBlock />
    </div>
  </section>;
}
