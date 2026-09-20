/*
 * The resolved data behind a `chart.series` block: the decoder, the request, and the arithmetic
 * a chart does before it draws (rebasing to 100, the "source data through" date).
 */

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const SERIES_DETAILS = Object.freeze(['full', 'summary'] as const);
export type SeriesDetail = (typeof SERIES_DETAILS)[number];

export const SERIES_VIEWS = Object.freeze(['line', 'normalized', 'bar', 'candles'] as const);
export type SeriesView = (typeof SERIES_VIEWS)[number];

/** `[YYYY-MM-DD, value]` — the summary's first and last points. */
const dateValueSchema = z.tuple([z.string(), z.number()]);

/**
 * A point row: `[ts_ms, value]`, or `[ts_ms, open, high, low, close, volume]` for `candles`.
 * `ts_ms` is UTC midnight.
 */
export const seriesPointSchema = z.array(z.number()).min(2);
export type SeriesPoint = z.infer<typeof seriesPointSchema>;

/** One asset that resolved; the numbers are the stored summary, and `points` arrive only on a `full` read. */
export const okSeriesEntrySchema = z.object({
  asset: z.string(),
  status: z.literal('ok'),
  currency: z.string().nullish(),
  /** The latest daily bar the source had published at resolution time. */
  complete_through: z.string(),
  n: z.number().int().nonnegative(),
  first: dateValueSchema,
  last: dateValueSchema,
  /** `(last - first) / first * 100`; `null` when `first` is zero. */
  change_pct: z.number().nullable(),
  high: z.number(),
  low: z.number(),
  points: z.array(seriesPointSchema).optional(),
});

/** An asset the source could not answer for (`unknown_asset`, `unavailable`). */
export const failedSeriesEntrySchema = z.object({
  asset: z.string(),
  status: z.string().refine((status) => status !== 'ok', { message: 'an ok entry carries its numbers' }),
  reason: z.string().nullish(),
});

export const seriesEntrySchema = z.union([okSeriesEntrySchema, failedSeriesEntrySchema]);
export type OkSeriesEntry = z.infer<typeof okSeriesEntrySchema>;
export type FailedSeriesEntry = z.infer<typeof failedSeriesEntrySchema>;
export type SeriesEntry = z.infer<typeof seriesEntrySchema>;

/* The block's presentation fields ride along so a renderer needs nothing but this object to draw. */
const presentationSchema = z.object({
  view: z.enum(SERIES_VIEWS),
  field: z.string(),
  period: z.string(),
  range: z.string(),
});

/**
 * `pending` is the absence of a row (`reason` says why when the read could not queue);
 * `unavailable` records a failed resolution.
 */
export const resolvedSeriesSchema = z.discriminatedUnion('status', [
  presentationSchema.extend({ status: z.literal('pending'), reason: z.string().nullish() }),
  presentationSchema.extend({ status: z.literal('unavailable'), reason: z.string(), resolved_at: z.string() }),
  presentationSchema.extend({
    status: z.literal('ok'),
    as_of: z.string(),
    resolved_at: z.string(),
    /** A frozen block whose source has published past `as_of`: immutable now. */
    pinned: z.boolean(),
    series: z.array(seriesEntrySchema),
  }),
]);
export type ResolvedSeries = z.infer<typeof resolvedSeriesSchema>;
export type OkResolvedSeries = Extract<ResolvedSeries, { status: 'ok' }>;

/** The 409 body: the block moved on since this document was read. */
export const staleRevBodySchema = z.object({ current_rev: z.number().int().nonnegative() });

/** What a block sees when it asks for its data; `stale-rev` is the 409 turned into a wait, not an error. */
export type SeriesResolution =
  | Readonly<{ status: 'loading' }>
  | Readonly<{ status: 'error'; message: string }>
  | Readonly<{ status: 'stale-rev'; current_rev: number }>
  | ResolvedSeries;

export function trackReportSeriesOperation(
  trackId: string, blockId: string, rev: number, detail: SeriesDetail,
): ApiOperation<ResolvedSeries> {
  const query = `rev=${encodeURIComponent(String(rev))}&detail=${encodeURIComponent(detail)}`;
  return {
    method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/report/series/${encodeURIComponent(blockId)}?${query}`,
    responseSchema: resolvedSeriesSchema,
  };
}

export function isOkSeriesEntry(entry: SeriesEntry): entry is OkSeriesEntry {
  return entry.status === 'ok';
}

/** Every series rebased so its first point reads 100; a first value that is not positive has no meaningful base. */
export type RebasedSeries =
  | Readonly<{ ok: true; points: readonly (readonly [number, number])[] }>
  | Readonly<{ ok: false; reason: 'first value ≤ 0' }>;

export function rebaseToFirst(points: readonly SeriesPoint[], valueIndex = 1): RebasedSeries {
  const first = points[0]?.[valueIndex];
  if (first === undefined || !(first > 0)) return { ok: false, reason: 'first value ≤ 0' };
  return {
    ok: true,
    points: points.map((point) => [point[0] ?? 0, ((point[valueIndex] ?? 0) / first) * 100] as const),
  };
}

/** The earliest `complete_through` across the resolved assets, or `null` when no asset resolved. */
export function minCompleteThrough(series: readonly SeriesEntry[]): string | null {
  let min: string | null = null;
  for (const entry of series) {
    if (!isOkSeriesEntry(entry)) continue;
    if (min === null || entry.complete_through < min) min = entry.complete_through;
  }
  return min;
}

/** The distinct currencies of the resolved assets, in first-seen order. */
export function seriesCurrencies(series: readonly SeriesEntry[]): string[] {
  const seen: string[] = [];
  for (const entry of series) {
    if (!isOkSeriesEntry(entry) || entry.currency == null || entry.currency === '') continue;
    if (!seen.includes(entry.currency)) seen.push(entry.currency);
  }
  return seen;
}
