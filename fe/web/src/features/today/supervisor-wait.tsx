import type { SupervisorSnapshot } from '../../../../core/domain/daily-planner.ts';
import { useState } from '../../ui/state/public.ts';
import { TimeSeriesChart, type PlotSelection } from '../../ui/data-visualization/public.tsx';
import styles from './supervisor-wait.module.css';

export function SupervisorWaitChart({ writes, observedAt, bucketMs, bucketCount }: Readonly<{
  writes: SupervisorSnapshot['database_writes'];
  observedAt: number;
  bucketMs: number;
  bucketCount: number;
}>) {
  const [selection, setSelection] = useState<PlotSelection>({ datasetId: 'write-wait', sample: null, selected: null, readoutOpen: false });
  const windowLabel = `最近 ${bucketMs * bucketCount / 60000} 分钟`;
  const end = Math.floor(observedAt / bucketMs) * bucketMs;
  const byTime = new Map(writes?.map(bucket => [bucket.started_at_ms, bucket.mean_wait_ms]));
  const timeLabel = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
  const points = Array.from({ length: bucketCount }, (_, index) => {
    const timestampMs = end - (bucketCount - 1 - index) * bucketMs;
    return { date: timeLabel.format(timestampMs), timestampMs, values: [byTime.get(timestampMs) ?? null] };
  });

  return <div className={styles.chart}>
    <TimeSeriesChart label={`SQLite 写事务等待 · ${windowLabel}`} emptyText={writes === null ? '暂不可用' : '该时段没有写事务观测'}
      datasets={[{ id: 'write-wait', label: windowLabel, unit: 'ms', style: 'line',
        series: [{ id: 'mean', label: '分桶平均等待', palette: 1 }], points }]}
      selection={selection} onSelection={setSelection} />
  </div>;
}
