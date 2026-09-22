import { z } from 'zod';

import { inlineTableBlockPayloadSchema } from './report.js';

function text(limit = 2048) {
  return z.string().refine((value) => [...value].length <= limit, 'Text exceeds the live-view limit');
}

const tone = z.enum(['neutral', 'positive', 'warning', 'negative']);
const identifier = z.string().min(1).max(200).regex(/\S/, 'Missing item identifier');
const timestamp = z.string().datetime({ offset: true });
const metric = z.strictObject({ label: text(120), value: text(120), detail: text(500), tone });
const notice = z.strictObject({ title: text(200), detail: text(), tone });
const bars = z.strictObject({
  kind: z.literal('bars'), title: text(200), unit: text(80), emptyText: text(500),
  points: z.array(z.strictObject({ label: text(200), value: z.number() })).max(24),
});
const budget = z.strictObject({
  kind: z.literal('budget'), title: text(200), unit: text(80), detail: text(500),
  used: z.number().nonnegative().nullable(), limit: z.number().positive().nullable(),
});

/** Pure, versioned plugin overlay data. It grants no write or navigation capability. */
export const reportLiveViewSchema = z.discriminatedUnion('view', [
  z.strictObject({
    version: z.literal(1), view: z.literal('overview'), asOf: timestamp.nullable(),
    metrics: z.array(metric).min(1).max(8), notices: z.array(notice).max(8),
    charts: z.array(z.discriminatedUnion('kind', [bars, budget])).max(4),
  }),
  z.strictObject({
    version: z.literal(1), view: z.literal('activity'), emptyText: text(500),
    items: z.array(z.strictObject({ id: identifier, at: timestamp, title: text(200), detail: text(), tone })).max(100)
      .refine((items) => new Set(items.map((item) => item.id)).size === items.length, 'Duplicate activity identifier'),
  }),
  z.strictObject({
    version: z.literal(1), view: z.literal('cards'), emptyText: text(500),
    items: z.array(z.strictObject({
      id: identifier, title: text(200), body: text(8000), next: text(8000), footer: text(500),
    })).max(50).refine((items) => new Set(items.map((item) => item.id)).size === items.length, 'Duplicate card identifier'),
  }),
  z.strictObject({
    version: z.literal(1), view: z.literal('details'), title: text(200),
    table: inlineTableBlockPayloadSchema,
  }),
]);

export type ReportLiveView = z.infer<typeof reportLiveViewSchema>;
export type OverviewChart = Extract<ReportLiveView, { view: 'overview' }>['charts'][number];

/** Balanced signed bars; a zero-only series remains an honest empty mark. */
export function signedBarLayout(values: readonly number[]) {
  const signed = values.some((value) => value < 0);
  const extent = Math.max(0, ...values.map(Math.abs));
  const zero = signed ? 50 : 0;
  return values.map((value) => {
    const width = extent === 0 ? 0 : Math.abs(value) / extent * (signed ? 50 : 100);
    return { start: value < 0 ? zero - width : zero, width, zero };
  });
}
