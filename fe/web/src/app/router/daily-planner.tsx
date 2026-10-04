// Reuse the Track route body; this composition only resolves dates and report evidence.
import type { ReactNode } from 'react';
import { useInfiniteQuery, useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { dailyTrackOperation, reportChangesOperation, reportEditsOperation, shiftDailyDate, isDailyDate, type ReportChange } from '../../../../core/domain/daily-planner.ts';
import { plannerCardIn } from '../../systems/cards/public.ts';
import { useConversationRegistry } from '../conversations/public.tsx';
import { DailyPage } from '../../features/today/daily.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { runOperation, trackDetailQueryOptions } from '../providers/queries.ts';

export function DailyTodayRoute({ transport, unauthorized, selectedDate, onSelectDate, onOpenTrack, legacy, renderTrack }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; selectedDate?: string;
  onSelectDate: (date?: string) => void; onOpenTrack: (trackId: string) => void;
  legacy: ReactNode; renderTrack: (detail: TrackDetailWire) => ReactNode;
}>) {
  const daily = useQuery({ queryKey: ['daily-planner', selectedDate ?? 'today'],
    queryFn: () => runOperation(transport, dailyTrackOperation(selectedDate), unauthorized), refetchInterval: 30_000 });
  const trackId = daily.data?.track_id;
  const detail = useQuery({ ...trackDetailQueryOptions(transport, trackId ?? '', unauthorized), enabled: trackId !== undefined });
  const registry = useConversationRegistry();
  const planner = plannerCardIn(detail.data?.cards ?? []);
  const date = daily.data?.date ?? (selectedDate !== undefined && isDailyDate(selectedDate) ? selectedDate : null);
  return <DailyPage date={date} timeZone={daily.data?.time_zone ?? null} onSelectDate={(next) => {
      if (next === undefined && selectedDate === undefined) void daily.refetch();
      onSelectDate(next);
    }}
    onOpenPlanner={planner === undefined ? undefined : () => registry.requestOpen(planner.id, { focusComposer: true })} legacy={legacy}
    changes={date === null ? null : <DailyReportChanges key={date} date={shiftDailyDate(date, -1)} transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />}>
    {daily.isError ? <ErrorBox message={daily.error.message} onRetry={() => { void daily.refetch(); }} />
      : daily.isPending ? <p role="status">Loading daily Planner…</p>
      : daily.data === null ? <p role="status">{selectedDate === undefined ? 'Preparing today’s Track…' : 'No daily Track was created for this date.'}</p>
      : detail.isError ? <ErrorBox message={detail.error.message} onRetry={() => { void detail.refetch(); }} />
      : detail.data === undefined || detail.data.track.id !== trackId ? <p role="status">Loading daily Track…</p>
      : renderTrack(detail.data)}
  </DailyPage>;
}

function DailyReportChanges({ date, transport, unauthorized, onOpenTrack }: Readonly<{
  date: string; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onOpenTrack: (trackId: string) => void;
}>) {
  const [open, setOpen] = useState(false);
  const changes = useInfiniteQuery({ queryKey: ['daily-report-changes', date], enabled: open,
    initialPageParam: undefined as Readonly<{ after: string; through: number }> | undefined,
    queryFn: ({ pageParam }) => runOperation(transport, reportChangesOperation(date, pageParam), unauthorized),
    getNextPageParam: (last) => last.next_cursor === null ? undefined : { after: last.next_cursor, through: last.through_event_id },
  });
  return <details onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Report changes · {date}</summary>
    {changes.isPending && open && <p role="status">Loading report changes…</p>}
    {changes.isError && <ErrorBox message={changes.error.message} onRetry={() => { void changes.refetch(); }} />}
    {!changes.isError && changes.data?.pages[0]?.changes.length === 0 && <p>No report changes recorded for visible Tracks on this day.</p>}
    {changes.data?.pages.flatMap((page) => page.changes.map((change) => <ReportChangeItem key={change.track_id} change={change}
      date={date} timeZone={page.time_zone} through={page.through_event_id} transport={transport} unauthorized={unauthorized} onOpenTrack={onOpenTrack} />))}
    {changes.hasNextPage && <button type="button" disabled={changes.isFetchingNextPage} onClick={() => { void changes.fetchNextPage(); }}>Load more reports</button>}
  </details>;
}

function ReportChangeItem({ change, date, timeZone, through, transport, unauthorized, onOpenTrack }: Readonly<{
  change: ReportChange; date: string; timeZone: string; through: number; transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel; onOpenTrack: (trackId: string) => void;
}>) {
  const [open, setOpen] = useState(false);
  const edits = useInfiniteQuery({ queryKey: ['daily-report-edits', date, change.track_id, through], enabled: open,
    initialPageParam: undefined as number | undefined,
    queryFn: ({ pageParam }) => runOperation(transport, reportEditsOperation(date, change.track_id, through, pageParam), unauthorized),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
  return <section aria-label={`Changes to ${change.track_title}`}>
    <button type="button" onClick={() => onOpenTrack(change.track_id)}>{change.area_name} / {change.track_title || 'Untitled Track'}</button>
    <p>{change.edit_count} report {change.edit_count === 1 ? 'edit' : 'edits'}</p>
    {change.summary_before !== change.summary_after && <p>Summary: {change.summary_before} → {change.summary_after}</p>}
    {change.patch === '' ? <p>No net body change; individual edits retain the history.</p> : <pre><code>{change.patch}</code></pre>}
    {change.patch_truncated && <p>Patch shortened. Read individual edits for the full content.</p>}
    <details onToggle={(event) => setOpen(event.currentTarget.open)}><summary>Individual edits</summary>
      {edits.isPending && open && <p role="status">Loading edits…</p>}
      {edits.isError && <ErrorBox message={edits.error.message} onRetry={() => { void edits.refetch(); }} />}
      {edits.data?.pages.flatMap((page) => page.edits.map((entry) => <details key={entry.event_id}>
        <summary>Report edit · {new Date(entry.at).toLocaleTimeString('en-GB', { timeZone, hour: '2-digit', minute: '2-digit', second: '2-digit' })}</summary>
        <h3>Before</h3><ReportDocument report={{ summary: entry.edit.summary_before, body: entry.edit.body_before, blocks: null }} empty={null}
          onOpenLink={(target) => onOpenTrack(target.trackId)} />
        <h3>After</h3><ReportDocument report={{ summary: entry.edit.summary_after, body: entry.edit.body_after, blocks: null }} empty={null}
          onOpenLink={(target) => onOpenTrack(target.trackId)} />
      </details>))}
      {edits.hasNextPage && <button type="button" disabled={edits.isFetchingNextPage} onClick={() => { void edits.fetchNextPage(); }}>Load more edits</button>}
    </details>
  </section>;
}
