import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from '../../ui/state/public.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { CALENDAR_PLUGIN_ID, calendarListOperation, calendarWriteOperation, shiftCalendarDate, type CalendarWrite } from '../../../../core/domain/calendar.ts';
import { CalendarTasks } from '../../features/calendar/public.tsx';
import { pluginsQueryOptions, runOperation } from '../providers/queries.ts';
import { useRecoveryMutation } from '../providers/recovery-mutation.ts';

export function TodayCalendarTasks({ date, transport, unauthorized, onSettings, onOpenTrack }: Readonly<{
  date: string; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onSettings(): void; onOpenTrack(id: string): void;
}>) {
  const [timezone] = useState(() => Intl.DateTimeFormat().resolvedOptions().timeZone);
  const client = useQueryClient();
  const plugins = useQuery(pluginsQueryOptions(transport, unauthorized));
  const enabled = plugins.data?.some((p) => p.id === CALENDAR_PLUGIN_ID && p.state === 'running') ?? false;
  const entries = useQuery({ queryKey: ['plugin-data', CALENDAR_PLUGIN_ID, date, timezone], enabled,
    queryFn: ({ signal }) => runOperation(transport, { ...calendarListOperation(date, shiftCalendarDate(date, 1), timezone), signal }, unauthorized) });
  const write = useRecoveryMutation(transport, {
    mutationFn: (input: CalendarWrite, admitted) => runOperation(admitted, calendarWriteOperation(input), unauthorized),
    onSuccess: () => { void client.invalidateQueries({ queryKey: ['plugin-data', CALENDAR_PLUGIN_ID] }); },
  });
  return <CalendarTasks date={date} timezone={timezone} entries={entries.data} enabled={enabled}
    loading={plugins.isPending || (enabled && entries.isPending)} error={plugins.error?.message ?? (enabled ? entries.error?.message : null) ?? null}
    pending={write.isPending} onSettings={onSettings} onOpenTrack={onOpenTrack}
    onRetry={() => { void plugins.refetch(); if (enabled) void entries.refetch(); }}
    onSave={async (input) => { await write.mutateAsync(input); }} />;
}
