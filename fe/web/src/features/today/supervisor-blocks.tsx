import type { ReactNode } from 'react';
import { CardFrame } from '../../ui/card-frame/public.tsx';
import styles from './supervisor-dashboard.module.css';

const unavailable = '暂不可用';

function SupervisorBlock({ title, children }: Readonly<{ title: string; children: ReactNode }>) {
  return <CardFrame elevation="flat" className={styles.block}>
    <section>
      <h3>{title}</h3>
      <div className={styles.content}>{children}</div>
    </section>
  </CardFrame>;
}

export function FindingsBlock() {
  return <SupervisorBlock title="需要处理">
    <p>异常巡检暂不可用</p>
    <p className={styles.muted}>执行停滞、未释放租约和清理失败</p>
  </SupervisorBlock>;
}

export function DeploymentBlock({ version, commit }: Readonly<{ version: string | undefined; commit: string | null | undefined }>) {
  return <SupervisorBlock title="部署与代码">
    <p className={styles.metric}>{version ?? unavailable}</p>
    <p>运行提交 {commit ? <code>{commit}</code> : unavailable}</p>
    <p className={styles.muted}>部署记录和远端 main 对比暂不可用</p>
  </SupervisorBlock>;
}

export function DatabaseBlock({ probe }: Readonly<{ probe: number | null | undefined }>) {
  return <SupervisorBlock title="DB 只读探针">
    <p className={styles.metric}>{probe == null ? unavailable : `${probe.toFixed(1)} ms`}</p>
    <p>最近一次探针</p>
    <p className={styles.muted}>24 小时趋势、p95 和备份状态暂不可用</p>
  </SupervisorBlock>;
}

export function TrackHealthBlock({ bootCompleted }: Readonly<{ bootCompleted: boolean | undefined }>) {
  const bootStatus = bootCompleted === undefined ? unavailable : bootCompleted ? '已完成' : '尚未完成';
  return <SupervisorBlock title="Track 健康">
    <p className={styles.metric}>{bootStatus}</p>
    <p>调度器启动扫描</p>
    <p className={styles.muted}>Track 运行、等待和停滞统计暂不可用</p>
  </SupervisorBlock>;
}

export function ResourcesBlock({ count, oldestRetirement }: Readonly<{ count: number | undefined; oldestRetirement: string | undefined }>) {
  return <SupervisorBlock title="资源台账">
    <p className={styles.metric}>{count === undefined ? unavailable : `${count} 个`}</p>
    <p>已退役工作区 · 按策略保留</p>
    {oldestRetirement !== undefined && <details>
      <summary>最早退役时间</summary>
      <p>{oldestRetirement} · Asia/Shanghai</p>
    </details>}
    <p className={styles.muted}>空间占用和清理失败统计暂不可用</p>
  </SupervisorBlock>;
}

export function InspectionBlock() {
  return <SupervisorBlock title="巡检记录">
    <p>历史巡检暂不可用</p>
    <p className={styles.muted}>每日记录与恢复时间线</p>
  </SupervisorBlock>;
}
