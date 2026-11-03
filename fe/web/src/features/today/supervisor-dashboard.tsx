import type { ReactNode } from 'react';
import type { SupervisorSnapshot } from '../../../../core/domain/daily-planner.ts';
import { PanelCard, PanelModule } from '../../ui/panel-card/public.tsx';
import styles from './supervisor-dashboard.module.css';

const unavailable = '暂不可用';

function observationTime(value: number): string {
  return new Date(value).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false });
}

export function SupervisorDashboard({ snapshot, status }: Readonly<{
  snapshot: SupervisorSnapshot | undefined;
  status: ReactNode;
}>) {
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
      <ResourcesBlock inventory={snapshot?.retired_workspaces} />
      <InspectionBlock />
    </div>
  </section>;
}

function SupervisorBlock({ title, children }: Readonly<{ title: string; children: ReactNode }>) {
  return <PanelCard className={styles.block}>
    <PanelModule title={title}><div className={styles.content}>{children}</div></PanelModule>
  </PanelCard>;
}

function FindingsBlock() {
  return <SupervisorBlock title="需要处理">
    <p>异常巡检暂不可用</p>
    <p className={styles.muted}>执行停滞、未释放租约和清理失败</p>
  </SupervisorBlock>;
}

function DeploymentBlock({ version, commit }: Readonly<{ version?: string; commit?: string | null }>) {
  return <SupervisorBlock title="部署与代码">
    <p className={styles.metric}>{version ?? unavailable}</p>
    <p>运行提交 {commit ? <code>{commit}</code> : unavailable}</p>
    <p className={styles.muted}>部署记录和远端 main 对比暂不可用</p>
  </SupervisorBlock>;
}

function DatabaseBlock({ probe }: Readonly<{ probe?: number | null }>) {
  return <SupervisorBlock title="DB 只读探针">
    <p className={styles.metric}>{probe == null ? unavailable : `${probe.toFixed(1)} ms`}</p>
    <p>最近一次探针</p>
    <p className={styles.muted}>24 小时趋势、p95 和备份状态暂不可用</p>
  </SupervisorBlock>;
}

function TrackHealthBlock({ bootCompleted }: Readonly<{ bootCompleted?: boolean }>) {
  const bootStatus = bootCompleted === undefined ? unavailable : bootCompleted ? '已完成' : '尚未完成';
  return <SupervisorBlock title="Track 健康">
    <p className={styles.metric}>{bootStatus}</p>
    <p>调度器启动扫描</p>
    <p className={styles.muted}>Track 运行、等待和停滞统计暂不可用</p>
  </SupervisorBlock>;
}

function ResourcesBlock({ inventory }: Readonly<{ inventory?: SupervisorSnapshot['retired_workspaces'] }>) {
  return <SupervisorBlock title="资源台账">
    <p className={styles.metric}>{inventory == null ? unavailable : `${inventory.count} 个`}</p>
    <p>已退役工作区 · 按策略保留</p>
    {inventory?.oldest_retired_at_ms != null && <details>
      <summary>最早退役时间</summary>
      <p>{observationTime(inventory.oldest_retired_at_ms)} · Asia/Shanghai</p>
    </details>}
    <p className={styles.muted}>空间占用和清理失败统计暂不可用</p>
  </SupervisorBlock>;
}

function InspectionBlock() {
  return <SupervisorBlock title="巡检记录">
    <p>历史巡检暂不可用</p>
    <p className={styles.muted}>每日记录与恢复时间线</p>
  </SupervisorBlock>;
}
