import { EmptyState } from '../../ui/empty-state/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import styles from './daily-planner.module.css';
// Reuse the Track route body; this composition only resolves dates and report evidence.
import type { ReactNode } from 'react';
import { useInfiniteQuery, useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { dailyTrackOperation, reportChangesOperation, reportEditsOperation, shiftDailyDate, supervisorSnapshotOperation, type ReportChange } from '../../../../core/domain/daily-planner.ts';
import { useCurrentRole, useCurrentUser } from '../auth/current-user.tsx';
import { useGo } from './navigation.ts';
import { TodayCalendarTasks } from './calendar.tsx';
import { ReportDetails } from '../../features/report/document/details.tsx';
import { ReportChangeDetails, ReportEditDetails } from '../../features/report/changes/public.tsx';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { runOperation, trackDetailQueryOptions } from '../providers/queries.ts';

export function DailyTodayRoute({ transport, unauthorized, selectedDate, onOpenTrack, renderTrack }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; selectedDate?: string;
  onOpenTrack: (trackId: string) => void;
  renderTrack: (detail: TrackDetailWire, evidence: ReactNode, panelContent: ReactNode, intro: ReactNode, supervisor: ReactNode) => ReactNode;
}>) {
  const daily = useQuery({ queryKey: ['daily-planner', selectedDate ?? 'today'],
    queryFn: () => runOperation(transport, dailyTrackOperation(selectedDate), unauthorized), refetchInterval: 30_000 });
  const trackId = daily.data?.track_id;
  const detail = useQuery({ ...trackDetailQueryOptions(transport, trackId ?? '', unauthorized), enabled: trackId !== undefined });
  if (daily.isError) return <ErrorBox message={readErrorText(daily.error, 'The daily Planner is unavailable.')} onRetry={() => { void daily.refetch(); }} />;
  if (daily.isPending) return <p role="status">Loading daily Planner…</p>;
  if (daily.data === null) return selectedDate === undefined ? <p role="status">Preparing today’s Track…</p> : <EmptyState variant="panel" icon={<Icon name="file" />} title="No daily Track was created for this date." description="这一天还没有日报记录。" />;
  if (detail.isError) return <ErrorBox message={readErrorText(detail.error, 'The daily Track could not be loaded.')} onRetry={() => { void detail.refetch(); }} />;
  if (detail.data === undefined || detail.data.track.id !== trackId) return <p role="status">Loading daily Track…</p>;
  const date = daily.data.date;
  return renderTrack({ ...detail.data, track: { ...detail.data.track, title: date } },
    <DailyReportChanges key={date} isToday={selectedDate === undefined} date={shiftDailyDate(date, -1)}
      transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />,
    <DailyCalendar key={date} date={date} transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />,
    <header className={styles.reportIntro}><h2>{selectedDate === undefined ? '今天的重点' : '当天的重点'}</h2><p>让工作更轻松，也更有条理。</p></header>,
    selectedDate === undefined && <DailySupervisor transport={transport} unauthorized={unauthorized} />);
}

function DailySupervisor({ transport, unauthorized }: Readonly<{ transport: ApiTransportPort; unauthorized: UnauthorizedChannel }>) {
  const user = useCurrentUser();
  const role = useCurrentRole();
  const [open, setOpen] = useState(false);
  const snapshot = useQuery({
    queryKey: ['daily-supervisor', user?.userId],
    queryFn: () => runOperation(transport, supervisorSnapshotOperation(), unauthorized),
    enabled: open && role === 'owner',
    refetchInterval: open ? 30_000 : false,
  });
  if (role !== 'owner') return null;

  const data = snapshot.isError ? undefined : snapshot.data;
  return <ReportDetails title="Daily Supervisor" layout="appendix" onToggle={setOpen}>
    {open && snapshot.isPending && <p role="status">正在读取实例状态…</p>}
    {snapshot.isError && <ErrorBox message="无法读取当前实例状态。" onRetry={() => { void snapshot.refetch(); }} />}
    {data !== undefined && <dl>
      <dt>运行版本</dt><dd>{data.kernel_version} · {data.build_sha ?? '提交未知'}</dd>
      <dt>调度器启动扫描</dt><dd>{data.scheduler_boot_sweep_completed ? '已完成' : '尚未完成'}</dd>
      <dt>DB 只读探针</dt><dd>{data.database_probe_ms === null ? '未知' : `${data.database_probe_ms.toFixed(1)} ms`}</dd>
      <dt>已退役的工作区</dt><dd>{data.retired_workspaces?.count ?? '未知'}</dd>
      {data.retired_workspaces?.oldest_retired_at_ms != null && <>
        <dt>最早退役时间</dt><dd>{new Date(data.retired_workspaces.oldest_retired_at_ms).toLocaleString()}</dd>
      </>}
      <dt>观测时间</dt><dd>{new Date(data.observed_at_ms).toLocaleString()}</dd>
    </dl>}
  </ReportDetails>;
}

function DailyCalendar({ date, transport, unauthorized, onOpenTrack }: Readonly<{
  date: string; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onOpenTrack(id: string): void;
}>) {
  const [selected, setSelected] = useState(date);
  const go = useGo();
  return <TodayCalendarTasks date={selected} onDateChange={setSelected} transport={transport}
    unauthorized={unauthorized} onSettings={() => go({ name: 'settings-plugins' })}
    onOpenTrack={onOpenTrack} />;
}

function DailyReportChanges({ date, isToday, transport, unauthorized, onOpenTrack }: Readonly<{
  date: string; isToday: boolean; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onOpenTrack: (trackId: string) => void;
}>) {
  const compactViewport = useCompactViewport();
  const [open, setOpen] = useState(false);
  const changes = useInfiniteQuery({ queryKey: ['daily-report-changes', date], enabled: open,
    initialPageParam: undefined as Readonly<{ cursor: string; through: number }> | undefined,
    queryFn: ({ pageParam }) => runOperation(transport, reportChangesOperation(date, pageParam), unauthorized),
    getNextPageParam: (last) => last.next_cursor === null ? undefined : { cursor: last.next_cursor, through: last.through_event_id },
  });
  return <ReportDetails title={compactViewport ? (isToday ? '昨天的更新' : '前一天的更新') : 'Report changes'} meta={date} layout="appendix" onToggle={setOpen}>
    {changes.isPending && open && <p role="status">Loading report changes…</p>}
    {changes.isError && <ErrorBox message={readErrorText(changes.error, 'Report changes could not be loaded.')} onRetry={() => { void changes.refetch(); }} />}
    {!changes.isError && changes.data?.pages[0]?.changes.length === 0 && <EmptyState variant="compact" icon={<Icon name="file" />} title="No report changes recorded for visible Tracks on this day." description="这一天的报告更新会显示在这里。" />}
    {changes.data?.pages.flatMap((page) => page.changes.map((change) => <ReportChangeItem key={change.track_id} change={change}
      date={date} timeZone={page.timezone} through={page.through_event_id} transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />))}
    {changes.hasNextPage && <button type="button" disabled={changes.isFetchingNextPage} onClick={() => { void changes.fetchNextPage(); }}>Load more reports</button>}
  </ReportDetails>;
}

function ReportChangeItem({ change, date, timeZone, through, transport, unauthorized, onOpenTrack }: Readonly<{
  change: ReportChange; date: string; timeZone: string; through: number; transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel; onOpenTrack: (trackId: string) => void;
}>) {
  const [open, setOpen] = useState(false);
  const edits = useInfiniteQuery({ queryKey: ['daily-report-edits', date, change.track_id, through], enabled: open,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => runOperation(transport, reportEditsOperation(date, change.track_id, through, pageParam), unauthorized),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
  return <ReportChangeDetails change={change} onOpenTrack={onOpenTrack} onToggleEdits={setOpen}>
    {edits.isPending && open && <p role="status">Loading edits…</p>}
    {edits.isError && <ErrorBox message={readErrorText(edits.error, 'Edits could not be loaded.')} onRetry={() => { void edits.refetch(); }} />}
    {edits.data?.pages.flatMap((page) => page.edits.map((entry) => <ReportEditDetails key={entry.event_id}
      entry={entry} timeZone={timeZone} onOpenTrack={onOpenTrack} />))}
    {edits.hasNextPage && <button type="button" disabled={edits.isFetchingNextPage} onClick={() => { void edits.fetchNextPage(); }}>Load more edits</button>}
  </ReportChangeDetails>;
}
