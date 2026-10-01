import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from '../../ui/state/public.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { CALENDAR_PLUGIN_ID, calendarListOperation, calendarWriteOperation, shiftCalendarDate, type CalendarWindow, type CalendarWrite } from '../../../../core/domain/calendar.ts';
import { CalendarTasks } from '../../features/calendar/public.tsx';
import { pluginsQueryOptions, runOperation } from '../providers/queries.ts';
import { useRecoveryMutation } from '../providers/recovery-mutation.ts';

export function TodayCalendarTasks({ date, onDateChange, trackCountOn, transport, unauthorized, onSettings, onOpenTrack }: Readonly<{
  trackCountOn?: (date: string) => number | null;
  date: string; onDateChange(date: string): void;
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onSettings(): void; onOpenTrack(id: string): void;
}>) {
  const [timezone] = useState(() => Intl.DateTimeFormat().resolvedOptions().timeZone);
  const [window, setWindow] = useState<CalendarWindow | null>(null);
  const client = useQueryClient();
  const plugins = useQuery(pluginsQueryOptions(transport, unauthorized));
  const enabled = plugins.data?.some((p) => p.id === CALENDAR_PLUGIN_ID && p.state === 'running') ?? false;
  const query = (range: CalendarWindow | null) => ({
    queryKey: ['plugin-data', CALENDAR_PLUGIN_ID, range?.from, range?.until, timezone], enabled: enabled && range !== null,
    queryFn: ({ signal }: { signal: AbortSignal }) => {
      if (range === null) throw new Error('The calendar has not reported its visible window.');
      return runOperation(transport, { ...calendarListOperation(range.from, range.until, timezone), signal }, unauthorized);
    },
  });
  const month = useQuery(query(window));
  const day = useQuery(query({ from: date, until: shiftCalendarDate(date, 1) }));
  const write = useRecoveryMutation(transport, {
    mutationFn: (input: CalendarWrite, admitted) => runOperation(admitted, calendarWriteOperation(input), unauthorized),
    onSuccess: () => { void client.invalidateQueries({ queryKey: ['plugin-data', CALENDAR_PLUGIN_ID] }); },
  });
  return <CalendarTasks trackCountOn={trackCountOn} date={date} timezone={timezone} enabled={enabled} pending={write.isPending}
    month={{ entries: month.data, loading: plugins.isPending || (enabled && month.isPending), error: plugins.error?.message ?? (enabled ? month.error?.message : null) ?? null }}
    day={{ entries: day.data, loading: enabled && day.isPending, error: enabled ? day.error?.message ?? null : null }}
    onDateChange={onDateChange} onWindowChange={(next) => setWindow((current) => current?.from === next.from && current.until === next.until ? current : next)}
    onSettings={onSettings} onOpenTrack={onOpenTrack}
    onRetry={() => { void plugins.refetch(); if (enabled) { if (window !== null) void month.refetch(); void day.refetch(); } }}
    onSave={async (input) => { await write.mutateAsync(input); }} />;
}
