import { Temporal } from 'temporal-polyfill';
import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';

export const CALENDAR_PLUGIN_ID = 'dev.neige.calendar';
export const CALENDAR_WEEKDAYS = Object.freeze(['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'] as const);
export const calendarScheduleSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('all_day'), date: z.string() }),
  z.object({ kind: z.literal('timed'), start: z.string(), end: z.string(), timezone: z.string() }),
  z.object({
    kind: z.literal('weekly'), weekdays: z.array(z.enum(CALENDAR_WEEKDAYS)), start: z.string(), end: z.string(),
    timezone: z.string(), from: z.string(), until: z.string().optional(),
  }),
]);
export const calendarDraftSchema = z.object({ title: z.string(), description: z.string(), schedule: calendarScheduleSchema });
export const calendarEntrySchema = z.object({
  id: z.string(), task: calendarDraftSchema, version: z.number(), cancelled: z.boolean(),
  source_track_id: z.string().nullable(), created_by: z.string(), created_at: z.number(), updated_at: z.number(),
});
/** The server's window projection: timed occurrences overlapping the listed window; none for all-day. */
const calendarOccurrenceSchema = z.object({ start: z.string(), end: z.string() });
export const calendarListedEntrySchema = z.object({ ...calendarEntrySchema.shape, occurrences: calendarOccurrenceSchema.array() });
export type CalendarOccurrence = z.infer<typeof calendarOccurrenceSchema>;
export type CalendarListedEntry = z.infer<typeof calendarListedEntrySchema>;
export type CalendarWindow = Readonly<{ from: string; until: string }>;
export type CalendarDraft = z.infer<typeof calendarDraftSchema>;
export type CalendarEntry = z.infer<typeof calendarEntrySchema>;
export type CalendarWrite = Readonly<{ idempotency_key: string; task: CalendarDraft }> |
  Readonly<{ id: string; expected_version: number; task: CalendarDraft; cancelled: boolean }>;
export function calendarListOperation(from: string, until: string, timezone: string): ApiOperation<CalendarListedEntry[]> {
  return { method: 'GET', path: `/api/calendar/tasks?from=${encodeURIComponent(from)}&until=${encodeURIComponent(until)}&timezone=${encodeURIComponent(timezone)}`, responseSchema: calendarListedEntrySchema.array() };
}
export function calendarWriteOperation(write: CalendarWrite): ApiOperation<CalendarEntry> {
  if ('id' in write) {
    const { id, ...body } = write;
    return { method: 'POST', path: `/api/calendar/tasks/${encodeURIComponent(id)}`, body, responseSchema: calendarEntrySchema };
  }
  return { method: 'POST', path: '/api/calendar/tasks', body: write, responseSchema: calendarEntrySchema };
}
export function calendarDate(now: number, timezone: string): string {
  return wallTime(now, timezone).slice(0, 10);
}
export function shiftCalendarDate(date: string, days: number): string {
  const value = new Date(`${date}T12:00:00Z`);
  value.setUTCDate(value.getUTCDate() + days);
  return value.toISOString().slice(0, 10);
}
export function wallTime(now: number, timezone: string): string {
  const parts = new Intl.DateTimeFormat('en-CA', { timeZone: timezone, year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).formatToParts(now);
  const part = (name: string) => parts.find((p) => p.type === name)?.value ?? '';
  return `${part('year')}-${part('month')}-${part('day')}T${part('hour')}:${part('minute')}`;
}
/** Resolve wall time explicitly; ambiguous DST inputs require an offset instead of silently choosing. */
export function calendarInstant(input: string, timezone: string): string {
  if (/([+-]\d\d:\d\d|Z)$/.test(input)) {
    if (!Number.isFinite(Date.parse(input))) throw new Error('Enter a valid date and time.');
    return input;
  }
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(input)) throw new Error('Use YYYY-MM-DDTHH:mm, optionally with an explicit UTC offset.');
  const base = Date.parse(`${input}:00Z`);
  if (!Number.isFinite(base) || new Date(base).toISOString().slice(0, 16) !== input) throw new Error('Enter a valid date and time.');
  const offsets = new Set<number>();
  for (let h = -36; h <= 36; h += 6) {
    const sample = base + h * 3600000;
    offsets.add(Date.parse(`${wallTime(sample, timezone)}:00Z`) - sample);
  }
  const matches = [...offsets].filter((offset) => wallTime(base - offset, timezone) === input);
  if (matches.length !== 1) throw new Error('This local time is missing or ambiguous because clocks change. Choose another time.');
  const minutes = matches[0] / 60000;
  const sign = minutes < 0 ? '-' : '+';
  const hours = String(Math.floor(Math.abs(minutes) / 60)).padStart(2, '0');
  const rest = String(Math.abs(minutes) % 60).padStart(2, '0');
  return `${input}:00${sign}${hours}:${rest}`;
}

/** The listed entry's first occurrence touching `day` in the display timezone; ends are exclusive and keep server precision. */
export function calendarOccurrenceOn(entry: CalendarListedEntry, day: string, timezone: string): CalendarOccurrence | undefined {
  const date = (instant: Temporal.Instant) => instant.toZonedDateTimeISO(timezone).toPlainDate().toString();
  return entry.occurrences.find((span) => date(Temporal.Instant.from(span.start)) <= day
    && date(Temporal.Instant.from(span.end).subtract({ nanoseconds: 1 })) >= day);
}
export function calendarEntryIncludesDate(entry: CalendarListedEntry, day: string, timezone: string): boolean {
  const schedule = entry.task.schedule;
  return schedule.kind === 'all_day' ? schedule.date === day : calendarOccurrenceOn(entry, day, timezone) !== undefined;
}
