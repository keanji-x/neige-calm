import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from '../../ui/state/public.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  CALENDAR_PLUGIN_ID, CALENDAR_WRITE_FAILURES, calendarListOperation, calendarReadBackWindow, calendarUpdateAttempts, calendarUpdateLanded,
  calendarWriteOperation, shiftCalendarDate, type CalendarDraft, type CalendarEdit, type CalendarWindow, type CalendarWrite,
} from '../../../../core/domain/calendar.ts';
import { writeClassOf } from '../../../../core/domain/failure-class.ts';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { CalendarTasks } from '../../features/calendar/public.tsx';
import { useKeyedIntent } from '../providers/idempotency-key.ts';
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
  const [updates] = useState(() => calendarUpdateAttempts());
  const write = useRecoveryMutation(transport, {
    mutationFn: async (input: CalendarWrite, admitted) => {
      const send = async () => { await runOperation(admitted, calendarWriteOperation(input), unauthorized); };
      if (!('id' in input)) return send();
      const { from, until, timezone: zone } = calendarReadBackWindow(input.task);
      return updates(input, send, async () => calendarUpdateLanded(
        await runOperation(admitted, calendarListOperation(from, until, zone), unauthorized), input,
      ));
    },
    /* `onSettled`: a write whose answer was lost may have been stored, and only the lists say so. */
    onSettled: () => { void client.invalidateQueries({ queryKey: ['plugin-data', CALENDAR_PLUGIN_ID] }); },
  });
  /* One new task is one `idempotency_key` (#2131): a retry of the same task after an unknown outcome resends the held
     key, so the server answers the task the first attempt made; a final outcome releases it. */
  const createIntent = useKeyedIntent<string, CalendarDraft>((held, next) => held === next);
  const save = async (edit: CalendarEdit) => {
    if ('id' in edit) { await write.mutateAsync(edit); return; }
    const request = createIntent.request(JSON.stringify(edit.task), () => edit.task);
    try {
      await write.mutateAsync({ idempotency_key: request.key, task: request.body });
    } catch (error) {
      if (writeClassOf(error, CALENDAR_WRITE_FAILURES.create) !== 'unknown') createIntent.release(request);
      throw error;
    }
    createIntent.release(request);
  };
  /* The month view also says when the plugin list (which decides whether the calendar runs) could not be read. */
  const monthError = plugins.error ?? (enabled ? month.error : null);
  return <CalendarTasks trackCountOn={trackCountOn} date={date} timezone={timezone} enabled={enabled} pending={write.isPending}
    month={{ entries: month.data, loading: plugins.isPending || (enabled && month.isPending), error: monthError === null ? null : readErrorText(monthError, 'Could not load the calendar.') }}
    day={{ entries: day.data, loading: enabled && day.isPending, error: enabled && day.error !== null ? readErrorText(day.error, 'Could not load this day.') : null }}
    onDateChange={onDateChange} onWindowChange={(next) => setWindow((current) => current?.from === next.from && current.until === next.until ? current : next)}
    onSettings={onSettings} onOpenTrack={onOpenTrack}
    onRetry={() => { void plugins.refetch(); if (enabled) { if (window !== null) void month.refetch(); void day.refetch(); } }}
    onSave={save} />;
}
