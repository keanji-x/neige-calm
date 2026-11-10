import { EmptyState } from '../../ui/empty-state/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import styles from './daily-planner.module.css';
// Reuse the Track route body; this composition only resolves dates and report evidence.
import { useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { useInfiniteQuery, useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { dailyTrackOperation, reportChangesOperation, reportEditsOperation, shiftDailyDate, supervisorSnapshotOperation, type ReportChange } from '../../../../core/domain/daily-planner.ts';
import { DEPLOYMENT_OWNER_USER_ID } from '../../../../core/api/auth.ts';
import { supervisorDetails, supervisorReport } from '../../../../core/domain/supervisor-view.ts';
import { SupervisorWaitChart } from '../../features/today/supervisor-wait.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { useCurrentUser } from '../auth/current-user.tsx';
import { useGo } from './navigation.ts';
import { TodayCalendarTasks } from './calendar.tsx';
import { ReportDetails } from '../../features/report/document/details.tsx';
import { ReportChangeDetails, ReportEditDetails } from '../../features/report/changes/public.tsx';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { runOperation, trackDetailQueryOptions } from '../providers/queries.ts';

type DailyCenterView = 'assistant' | 'changes' | 'supervisor';

export type DailyCenterPresentation = Readonly<{
  title: string;
  presented: boolean;
  onOpenAssistant: () => void;
  render: (assistant: ReactNode, guideHost: HTMLDivElement | null) => ReactNode;
}>;

export function DailyTodayRoute({ transport, unauthorized, selectedDate, onOpenTrack, renderTrack }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; selectedDate?: string;
  onOpenTrack: (trackId: string) => void;
  renderTrack: (detail: TrackDetailWire, evidence: ReactNode, panelContent: ReactNode, intro: ReactNode, supervisor: ReactNode, center?: DailyCenterPresentation) => ReactNode;
}>) {
  const [view, setView] = useState<DailyCenterView>('assistant');
  const user = useCurrentUser();
  const daily = useQuery({ queryKey: ['daily-planner', selectedDate ?? 'today'],
    queryFn: () => runOperation(transport, dailyTrackOperation(selectedDate), unauthorized), refetchInterval: 30_000 });
  const trackId = daily.data?.track_id;
  const detail = useQuery({ ...trackDetailQueryOptions(transport, trackId ?? '', unauthorized), enabled: trackId !== undefined });
  const supervisor = selectedDate === undefined && <DailySupervisor transport={transport} unauthorized={unauthorized} />;
  const withSupervisor = (content: ReactNode) => <>{supervisor}{content}</>;
  if (daily.isError) return withSupervisor(<ErrorBox message={readErrorText(daily.error, 'The daily Planner is unavailable.')} onRetry={() => { void daily.refetch(); }} />);
  if (daily.isPending) return withSupervisor(<p role="status">Loading daily Planner…</p>);
  if (daily.data === null) return withSupervisor(selectedDate === undefined ? <p role="status">Preparing today’s Track…</p> : <EmptyState variant="panel" icon={<Icon name="file" />} title="No daily Track was created for this date." description="这一天还没有日报记录。" />);
  if (detail.isError) return withSupervisor(<ErrorBox message={readErrorText(detail.error, 'The daily Track could not be loaded.')} onRetry={() => { void detail.refetch(); }} />);
  if (detail.data === undefined || detail.data.track.id !== trackId) return withSupervisor(<p role="status">Loading daily Track…</p>);
  const date = daily.data.date;
  return renderTrack({ ...detail.data, track: { ...detail.data.track, title: date } },
    <DailyReportChanges key={date} isToday={selectedDate === undefined} date={shiftDailyDate(date, -1)}
      transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />,
    <DailyCalendar key={date} date={date} transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />,
    <header className={styles.reportIntro}><h2>{selectedDate === undefined ? '今天的重点' : '当天的重点'}</h2><p>让工作更轻松，也更有条理。</p></header>,
    supervisor,
    selectedDate === undefined ? { title: 'Today Assistant', presented: view === 'assistant', onOpenAssistant: () => setView('assistant'), render: (assistant, guideHost) => <TodayCenter view={view} onView={setView} assistant={assistant} guideHost={guideHost}
      changes={<DailyReportChanges key={date} standalone isToday date={shiftDailyDate(date, -1)} transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />}
      supervisor={user?.userId === DEPLOYMENT_OWNER_USER_ID ? supervisor : undefined} /> } : undefined);
}

/** View state is visit-local; keep the live conversation mounted so its draft and history survive. */
function TodayCenter({ assistant, changes, supervisor, guideHost, view, onView }: Readonly<{ view: DailyCenterView; onView: (view: DailyCenterView) => void; guideHost: HTMLDivElement | null; assistant: ReactNode; changes: ReactNode; supervisor?: ReactNode }>) {
  const previous = useRef(view);
  const back = useRef<HTMLButtonElement>(null);
  const changesGuide = useRef<HTMLButtonElement>(null);
  const supervisorGuide = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => {
    if (previous.current === view) return;
    const from = previous.current;
    previous.current = view;
    (view !== 'assistant' ? back.current : from === 'changes' ? changesGuide.current : supervisorGuide.current)?.focus();
  }, [view]);
  const guide = <nav className={styles.guide} aria-label="Today views">
      <button ref={changesGuide} type="button" onClick={() => onView('changes')}>Report changes</button>
      {supervisor !== undefined && <button ref={supervisorGuide} type="button" onClick={() => onView('supervisor')}>Daily Supervisor</button>}
    </nav>;
  return <div className={styles.center}>
    <div className={styles.assistant} hidden={view !== 'assistant'}>{assistant}</div>
    {guideHost !== null && createPortal(guide, guideHost)}
    {view !== 'assistant' && <section className={styles.subview} aria-label={view === 'changes' ? 'Report changes' : 'Daily Supervisor view'}>
      <button ref={back} type="button" className={styles.backToToday} onClick={() => onView('assistant')}><Icon name="arrow-left" size="sm" />Back to Today Assistant</button>
      {view === 'changes' ? changes : supervisor}
    </section>}
  </div>;
}

function DailySupervisor({ transport, unauthorized }: Readonly<{ transport: ApiTransportPort; unauthorized: UnauthorizedChannel }>) {
  const user = useCurrentUser();
  const isOwner = user?.userId === DEPLOYMENT_OWNER_USER_ID;
  const snapshot = useQuery({
    queryKey: ['daily-supervisor', user?.userId],
    queryFn: () => runOperation(transport, supervisorSnapshotOperation(), unauthorized),
    enabled: isOwner,
    refetchInterval: isOwner ? 30_000 : false,
  });
  if (!isOwner) return null;

  if (snapshot.isError) return <ErrorBox message="无法读取当前实例状态。" onRetry={() => { void snapshot.refetch(); }} />;
  if (snapshot.data === undefined) return <p role="status">正在读取实例状态…</p>;

  return <section aria-label="Daily Supervisor">
    <ReportDocument report={supervisorReport(snapshot.data)} empty={null} />
    <SupervisorWaitChart writes={snapshot.data.database_writes} observedAt={snapshot.data.observed_at_ms}
      bucketMs={snapshot.data.write_bucket_ms} bucketCount={snapshot.data.write_bucket_count} />
    <ReportDetails title="实例明细" layout="appendix">
      <dl>{supervisorDetails(snapshot.data).map(({ label, value }) =>
        <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>
    </ReportDetails>
  </section>;
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

function DailyReportChanges({ date, isToday, transport, unauthorized, onOpenTrack, standalone = false }: Readonly<{
  date: string; isToday: boolean; standalone?: boolean; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onOpenTrack: (trackId: string) => void;
}>) {
  const compactViewport = useCompactViewport();
  const [open, setOpen] = useState(standalone);
  const changes = useInfiniteQuery({ queryKey: ['daily-report-changes', date], enabled: open,
    initialPageParam: undefined as Readonly<{ cursor: string; through: number }> | undefined,
    queryFn: ({ pageParam }) => runOperation(transport, reportChangesOperation(date, pageParam), unauthorized),
    getNextPageParam: (last) => last.next_cursor === null ? undefined : { cursor: last.next_cursor, through: last.through_event_id },
  });
  const content = <>
    {changes.isPending && open && <p role="status">Loading report changes…</p>}
    {changes.isError && <ErrorBox message={readErrorText(changes.error, 'Report changes could not be loaded.')} onRetry={() => { void changes.refetch(); }} />}
    {!changes.isError && changes.data?.pages[0]?.changes.length === 0 && <EmptyState variant="compact" icon={<Icon name="file" />} title="No report changes recorded for visible Tracks on this day." description="这一天的报告更新会显示在这里。" />}
    {changes.data?.pages.flatMap((page) => page.changes.map((change) => <ReportChangeItem key={change.track_id} card={standalone} change={change}
      date={date} timeZone={page.timezone} through={page.through_event_id} transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />))}
    {changes.hasNextPage && <button type="button" disabled={changes.isFetchingNextPage} onClick={() => { void changes.fetchNextPage(); }}>Load more reports</button>}
  </>;
  if (standalone) return <><h2 className={styles.subviewTitle}>Report changes · {date}</h2>{content}</>;
  return <ReportDetails title={compactViewport ? (isToday ? '昨天的更新' : '前一天的更新') : 'Report changes'} meta={date} layout="appendix" onToggle={setOpen}>
    {open && <div className={styles.subviewHeader}>
      <button type="button" className={styles.backToToday} onClick={(event) => {
        const details = event.currentTarget.closest('details');
        if (details !== null) details.open = false;
        setOpen(false);
      }}>
        <Icon name="arrow-left" size="sm" />
        <span>Back to Today</span>
      </button>
    </div>}
    {content}
  </ReportDetails>;
}

function ReportChangeItem({ card, change, date, timeZone, through, transport, unauthorized, onOpenTrack }: Readonly<{
  card?: boolean; change: ReportChange; date: string; timeZone: string; through: number; transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel; onOpenTrack: (trackId: string) => void;
}>) {
  const [open, setOpen] = useState(false);
  const edits = useInfiniteQuery({ queryKey: ['daily-report-edits', date, change.track_id, through], enabled: open,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => runOperation(transport, reportEditsOperation(date, change.track_id, through, pageParam), unauthorized),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
  return <ReportChangeDetails card={card} change={change} onOpenTrack={onOpenTrack} onToggleEdits={setOpen}>
    {edits.isPending && open && <p role="status">Loading edits…</p>}
    {edits.isError && <ErrorBox message={readErrorText(edits.error, 'Edits could not be loaded.')} onRetry={() => { void edits.refetch(); }} />}
    {edits.data?.pages.flatMap((page) => page.edits.map((entry) => <ReportEditDetails key={entry.event_id}
      entry={entry} timeZone={timeZone} onOpenTrack={onOpenTrack} />))}
    {edits.hasNextPage && <button type="button" disabled={edits.isFetchingNextPage} onClick={() => { void edits.fetchNextPage(); }}>Load more edits</button>}
  </ReportChangeDetails>;
}
