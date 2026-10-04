// Daily identity is resolved by the server; the browser never creates a Track on page load.
import { z } from 'zod';
import type { ApiOperation } from '../api/types.ts';

export const dailyTrackSchema = z.object({ date: z.string().refine(isDailyDate), time_zone: z.string(), track_id: z.string() });
export type DailyTrack = z.infer<typeof dailyTrackSchema>;
export const reportChangeSchema = z.object({
  track_id: z.string(), track_title: z.string(), area_id: z.string(), area_name: z.string(),
  edit_count: z.number(), first_event_id: z.number(), last_event_id: z.number(),
  summary_before: z.string(), summary_after: z.string(), patch: z.string(), patch_truncated: z.boolean(),
});
export type ReportChange = z.infer<typeof reportChangeSchema>;
const reportChangesSchema = z.object({
  date: z.string(), time_zone: z.string(), through_event_id: z.number(),
  changes: z.array(reportChangeSchema), next_cursor: z.string().nullable(),
});
export type ReportChangesPage = z.infer<typeof reportChangesSchema>;
const reportEditsSchema = z.object({
  edits: z.array(z.object({ event_id: z.number(), at: z.number(), edit: z.object({
    track_id: z.string(), edit_id: z.string(), summary_before: z.string(), summary_after: z.string(),
    body_before: z.string(), body_after: z.string(),
  }) })), next_cursor: z.number().nullable(),
});
export type ReportEditsPage = z.infer<typeof reportEditsSchema>;

export function dailyTrackOperation(date?: string): ApiOperation<DailyTrack | null> {
  return { method: 'GET', path: `/api/today/daily${date === undefined ? '' : `?date=${encodeURIComponent(date)}`}`, responseSchema: dailyTrackSchema.nullable() };
}
export function reportChangesOperation(date: string, cursor?: Readonly<{ after: string; through: number }>): ApiOperation<ReportChangesPage> {
  const suffix = cursor === undefined ? '' : `&after=${encodeURIComponent(cursor.after)}&through_event_id=${cursor.through}`;
  return { method: 'GET', path: `/api/today/report-changes?date=${encodeURIComponent(date)}${suffix}`, responseSchema: reportChangesSchema };
}
export function reportEditsOperation(date: string, trackId: string, through: number, after?: number): ApiOperation<ReportEditsPage> {
  const suffix = after === undefined ? '' : `&after=${after}`;
  return { method: 'GET', path: `/api/today/report-edits?date=${encodeURIComponent(date)}&track_id=${encodeURIComponent(trackId)}&through_event_id=${through}${suffix}`, responseSchema: reportEditsSchema };
}

/** Date-only arithmetic, independent of the browser's local zone. */
export function shiftDailyDate(date: string, days: number): string {
  const value = new Date(`${date}T12:00:00Z`);
  value.setUTCDate(value.getUTCDate() + days);
  return value.toISOString().slice(0, 10);
}

export function isDailyDate(date: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) return false;
  const parsed = new Date(`${date}T12:00:00Z`);
  return Number.isFinite(parsed.getTime()) && parsed.toISOString().slice(0, 10) === date;
}
