import { z } from 'zod';

import { inlineTableBlockPayloadSchema } from './report.js';

function text(limit = 2048) {
  return z.string().refine((value) => [...value].length <= limit, 'Text exceeds the live-view limit');
}

const tone = z.enum(['neutral', 'positive', 'warning', 'negative']);
const identifier = z.string().min(1).max(200).regex(/\S/, 'Missing item identifier');
const timestamp = z.string().max(128).datetime({ offset: true });
const metric = z.strictObject({ label: text(120), value: text(120), detail: text(500), tone });
const notice = z.strictObject({ title: text(200), detail: text(), tone });
const bars = z.strictObject({
  kind: z.literal('bars'), title: text(200), unit: text(80), emptyText: text(500),
  points: z.array(z.strictObject({ label: text(200), value: z.number(), tone })).max(24),
});
const meter = z.strictObject({
  kind: z.literal('meter'), title: text(200), unit: text(80), detail: text(500),
  used: z.number().nonnegative().nullable(), limit: z.number().positive().nullable(),
  usedLabel: text(120), limitLabel: text(120), emptyText: text(500), tone,
});

/** Pure, versioned plugin overlay data. It grants no write or navigation capability. */
export const reportLiveViewSchema = z.discriminatedUnion('view', [
  z.strictObject({
    version: z.literal(1), view: z.literal('overview'),
    updated: z.strictObject({ label: text(120), at: timestamp }).nullable(),
    metrics: z.array(metric).min(1).max(8), notices: z.array(notice).max(8),
    charts: z.array(z.discriminatedUnion('kind', [bars, meter])).max(4),
  }),
  z.strictObject({
    version: z.literal(1), view: z.literal('activity'), emptyText: text(500),
    items: z.array(z.strictObject({ id: identifier, at: timestamp, title: text(200), detail: text(), tone })).max(100)
      .refine((items) => new Set(items.map((item) => item.id)).size === items.length, 'Duplicate activity identifier'),
  }),
  z.strictObject({
    version: z.literal(1), view: z.literal('cards'), emptyText: text(500),
    items: z.array(z.strictObject({
      id: identifier, title: text(200), body: text(8000), footer: text(500),
      sections: z.array(z.strictObject({ label: text(120), body: text(8000) })).max(4),
    })).max(50).refine((items) => new Set(items.map((item) => item.id)).size === items.length, 'Duplicate card identifier'),
  }),
  z.strictObject({
    version: z.literal(1), view: z.literal('details'), title: text(200),
    table: inlineTableBlockPayloadSchema,
  }),
]).refine((value) => {
  // JSON.stringify escapes lone surrogates; every remaining code point is valid UTF-8.
  let bytes = 0;
  for (const char of JSON.stringify(value)) {
    const code = char.codePointAt(0)!;
    bytes += code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
    if (bytes > 4 * 1024 * 1024) return false;
  }
  return true;
}, 'Live view exceeds 4 MiB');

export type ReportLiveView = z.infer<typeof reportLiveViewSchema>;
export type OverviewChart = Extract<ReportLiveView, { view: 'overview' }>['charts'][number];
