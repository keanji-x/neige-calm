import type { SupervisorSnapshot } from './daily-planner.js';
import type { TrackReport } from './report.js';
import type { Metric } from './report-view.types.generated.js';

const unavailable = '暂不可用';

function count(value: number): Metric['value'] {
  return { state: 'known', amount: value, unit: '', placement: 'suffix', decimals: 0, signed: false };
}

/** Only count explicit checks reported by the endpoint. */
function healthMetrics(snapshot: SupervisorSnapshot): Metric[] {
  const bootCompleted = snapshot.scheduler_boot_sweep_completed;
  return [
    {
      id: 'not-passed', label: '未通过', value: count(Number(!bootCompleted)),
      detail: bootCompleted ? '已观测的检查项' : '调度器启动扫描尚未完成',
      tone: bootCompleted ? 'neutral' : 'warning', emphasis: 'normal',
    },
    {
      id: 'passed', label: '通过', value: count(Number(bootCompleted)),
      detail: '当前覆盖调度器启动扫描', tone: 'neutral', emphasis: 'normal',
    },
  ];
}

/** A read-only report projection. It is never committed to the daily Track. */
export function supervisorReport(snapshot: SupervisorSnapshot): TrackReport {
  const writes = snapshot.database_writes;
  const failures = writes?.reduce((total, bucket) => total + bucket.failures, 0);
  const retries = writes?.reduce((total, bucket) => total + bucket.retries, 0);
  return {
    summary: '',
    body: '',
    blocks: [{
      id: 'supervisor-observations',
      kind: 'view',
      payload: {
        version: 1,
        title: 'Daily Supervisor',
        description: '',
        snapshot: { id: 'instance-observation', observedAt: snapshot.observed_at_ms, producedAt: null },
        rows: [
          {
            id: 'checks', title: '', layout: 'one',
            cells: [{ kind: 'metrics', id: 'health', title: '健康检查', items: healthMetrics(snapshot) }],
          },
          {
            id: 'operations', title: '', layout: 'two', cells: [
              {
                kind: 'metrics', id: 'resources', title: '资源',
                items: [
                  {
                    id: 'unreleased', label: '待清理 Worker 会话',
                    value: snapshot.settled_worker_cleanup_candidates === null
                      ? { state: 'unknown', reason: unavailable }
                      : count(snapshot.settled_worker_cleanup_candidates),
                    detail: '任务结束或 Track 关闭后的清理候选', tone: 'neutral', emphasis: 'normal',
                  },
                  {
                    id: 'retired', label: '已退役工作区',
                    value: snapshot.retired_workspaces === null
                      ? { state: 'unknown', reason: unavailable }
                      : count(snapshot.retired_workspaces.count),
                    detail: '按策略保留', tone: 'neutral', emphasis: 'normal',
                  },
                ],
              },
              {
                kind: 'metrics', id: 'write-outcomes', title: 'SQLite 写事务',
                items: [
                  {
                    id: 'failures', label: '事务阶段报错',
                    value: failures === undefined ? { state: 'unknown', reason: unavailable } : count(failures),
                    detail: '连接获取、BEGIN、提交或回滚报错',
                    tone: failures ? 'warning' : 'neutral', emphasis: 'normal',
                  },
                  {
                    id: 'retries', label: '内建启动重试',
                    value: retries === undefined ? { state: 'unknown', reason: unavailable } : count(retries),
                    detail: '仅计入 SQLite 写入助手的重试', tone: 'neutral', emphasis: 'normal',
                  },
                ],
              },
            ],
          },
        ],
      },
    }],
  };
}

export function supervisorDetails(snapshot: SupervisorSnapshot) {
  const oldest = snapshot.retired_workspaces?.oldest_retired_at_ms;
  return [
    { label: '运行版本', value: snapshot.kernel_version },
    { label: '运行提交', value: snapshot.build_sha ?? unavailable },
    ...(oldest == null ? [] : [{
      label: '最早退役时间',
      value: new Date(oldest).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false }),
    }]),
    { label: '历史巡检', value: unavailable },
    { label: '写事务等待口径', value: '连接获取、BEGIN 与退避耗时之和。仅保留本次启动的近期观测。' },
  ];
}
