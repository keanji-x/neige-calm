// Reuse the Track route body; this composition only resolves dates and report evidence.
import type { ReactNode } from 'react';
import { useInfiniteQuery, useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { dailyTrackOperation, reportChangesOperation, reportEditsOperation, shiftDailyDate, type ReportChange } from '../../../../core/domain/daily-planner.ts';
import { useGo } from './navigation.ts';
import { TodayCalendarTasks } from './calendar.tsx';
import { PanelCard } from '../../ui/panel-card/public.tsx';
import { ReportDetails } from '../../features/report/document/details.tsx';
import { ReportChangeDetails, ReportEditDetails } from '../../features/report/changes/public.tsx';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { runOperation, trackDetailQueryOptions } from '../providers/queries.ts';

export function DailyTodayRoute({ transport, unauthorized, selectedDate, onOpenTrack, renderTrack }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; selectedDate?: string;
  onOpenTrack: (trackId: string) => void;
  renderTrack: (detail: TrackDetailWire, evidence: ReactNode, leadingContent: ReactNode) => ReactNode;
}>) {
  const daily = useQuery({ queryKey: ['daily-planner', selectedDate ?? 'today'],
    queryFn: () => runOperation(transport, dailyTrackOperation(selectedDate), unauthorized), refetchInterval: 30_000 });
  const trackId = daily.data?.track_id;
  const detail = useQuery({ ...trackDetailQueryOptions(transport, trackId ?? '', unauthorized), enabled: trackId !== undefined });
  if (daily.isError) return <ErrorBox message={readErrorText(daily.error, 'The daily Planner is unavailable.')} onRetry={() => { void daily.refetch(); }} />;
  if (daily.isPending) return <p role="status">Loading daily Planner…</p>;
  if (daily.data === null) return <p role="status">{selectedDate === undefined ? 'Preparing today’s Track…' : 'No daily Track was created for this date.'}</p>;
  if (detail.isError) return <ErrorBox message={readErrorText(detail.error, 'The daily Track could not be loaded.')} onRetry={() => { void detail.refetch(); }} />;
  if (detail.data === undefined || detail.data.track.id !== trackId) return <p role="status">Loading daily Track…</p>;
  const date = daily.data.date;
  return renderTrack({ ...detail.data, track: { ...detail.data.track, title: date } },
    <DailyReportChanges key={date} date={shiftDailyDate(date, -1)}
      transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />,
    <DailyCalendar key={date} date={date} transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />);
}

function DailyCalendar({ date, transport, unauthorized, onOpenTrack }: Readonly<{
  date: string; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onOpenTrack(id: string): void;
}>) {
  const [selected, setSelected] = useState(date);
  const go = useGo();
  return <PanelCard><TodayCalendarTasks date={selected} onDateChange={setSelected} transport={transport}
    unauthorized={unauthorized} onSettings={() => go({ name: 'settings-plugins' })}
    onOpenTrack={onOpenTrack} /></PanelCard>;
}

function DailyReportChanges({ date, transport, unauthorized, onOpenTrack }: Readonly<{
  date: string; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onOpenTrack: (trackId: string) => void;
}>) {
  const [open, setOpen] = useState(false);
  const changes = useInfiniteQuery({ queryKey: ['daily-report-changes', date], enabled: open,
    initialPageParam: undefined as Readonly<{ cursor: string; through: number }> | undefined,
    queryFn: ({ pageParam }) => runOperation(transport, reportChangesOperation(date, pageParam), unauthorized),
    getNextPageParam: (last) => last.next_cursor === null ? undefined : { cursor: last.next_cursor, through: last.through_event_id },
  });
  return <ReportDetails title="Report changes" meta={date} layout="appendix" onToggle={setOpen}>
    {changes.isPending && open && <p role="status">Loading report changes…</p>}
    {changes.isError && <ErrorBox message={readErrorText(changes.error, 'Report changes could not be loaded.')} onRetry={() => { void changes.refetch(); }} />}
    {!changes.isError && changes.data?.pages[0]?.changes.length === 0 && <p>No report changes recorded for visible Tracks on this day.</p>}
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
